use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use serde_json::{Value, json};
use tracedecay_domain::ManifestDigest;
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
    DiagnosticSnapshotAdapter, MAX_DIAGNOSTIC_OPERATIONS, OverlaySnapshot,
};
use crate::protocol::{DaemonLspProtocolSession, MAX_QUEUED_OUTBOUND_MESSAGES};
use crate::provider::{
    DiagnosticRefreshAdmission, DiagnosticSnapshotOutcome, DiagnosticSnapshotPort,
    DiagnosticSnapshotValidation, GenerationDiagnostics,
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
    authority_digest: Arc<Mutex<Option<ManifestDigest>>>,
    validation_pending: Arc<AtomicBool>,
    validation_failure: Arc<Mutex<Option<&'static str>>>,
}

impl CanonicalDiagnosticSnapshotAuthority for DiagnosticAuthority {
    fn current_authority(
        &self,
        _request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<ManifestDigest, LspRuntimeFailure>> {
        let digest = Arc::clone(&self.authority_digest);
        let pending = Arc::clone(&self.validation_pending);
        let failure = Arc::clone(&self.validation_failure);
        Box::pin(std::future::poll_fn(move |_| {
            if pending.load(Ordering::SeqCst) {
                Poll::Pending
            } else if let Some(failure) = failure.lock().unwrap().take() {
                Poll::Ready(Err(LspRuntimeFailure::new(failure)))
            } else {
                Poll::Ready(Ok(digest
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| tracedecay_domain::test_fixtures::digest('a'))))
            }
        }))
    }

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
        let authority_digest = self
            .authority_digest
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| digest('a'));
        Box::pin(async move {
            if let Some(failure) = failure {
                return Err(LspRuntimeFailure::new(failure));
            }
            Ok(GenerationDiagnostics {
                generation,
                authority_digest,
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

    fn validate_document_snapshot(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
        snapshot: &GenerationDiagnostics,
    ) -> DiagnosticSnapshotValidation {
        self.adapter
            .validate_document_snapshot(root, document_uri, overlay, snapshot)
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
    message_duration_ms: u64,
    next_request_id: u64,
}

impl Harness {
    fn new(push: bool) -> Self {
        Self::with_refresh_support(push, false)
    }

    fn with_refresh_support(push: bool, refresh_support: bool) -> Self {
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
            message_duration_ms: 1,
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
        if refresh_support {
            capabilities["workspace"]["diagnostic"] = json!({ "refreshSupport": true });
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
        assert_eq!(
            harness
                .session
                .lifecycle
                .gateway
                .capabilities()
                .supports_workspace_diagnostic_refresh,
            refresh_support,
        );
        harness
    }

    fn send(&mut self, value: Value) -> Vec<Value> {
        self.now_ms += self.message_duration_ms;
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
fn retained_reads_revalidate_authority_before_returning_unchanged() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    let old = assert_full(&harness.pull(None), 1, 1);
    *harness.authority.authority_digest.lock().unwrap() = Some(digest('b'));
    *harness.authority.next_generation.lock().unwrap() = Some(1);
    let stale = harness.pull(Some(&old));
    assert_eq!(stale["error"]["code"], -32802, "{stale}");
    assert!(stale.get("result").is_none());
    harness.runtime.complete_next();
    let current = assert_full(&harness.pull(Some(&old)), 1, 1);
    assert_ne!(current, old);
    assert_eq!(harness.pull(Some(&current))["result"]["kind"], "unchanged");
    assert_eq!(harness.runtime.started(), 2);
}

#[test]
fn changed_authority_discards_queued_stale_publications() {
    let mut harness = Harness::new(true);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    harness.session.flush_due(harness.now_ms);
    *harness.authority.authority_digest.lock().unwrap() = Some(digest('b'));
    let messages = harness.send(json!({
        "jsonrpc": "2.0", "id": 999, "method": "textDocument/diagnostic",
        "params": { "textDocument": { "uri": DOCUMENT } },
    }));
    assert!(
        messages
            .iter()
            .any(|message| { message["id"] == 999 && message["error"]["code"] == -32802 }),
        "{messages:?}"
    );
    assert!(
        !messages.iter().any(|message| {
            message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == DOCUMENT
        }),
        "{messages:?}"
    );
}

#[test]
fn pending_authority_validation_is_not_consumed_by_background_publication() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    let result = assert_full(&harness.pull(None), 1, 1);
    harness
        .authority
        .validation_pending
        .store(true, Ordering::SeqCst);
    let pending = harness.pull(Some(&result));
    assert_eq!(pending["error"]["code"], -32802, "{pending}");
    assert!(pending.get("result").is_none());
    harness.session.flush_due(harness.now_ms);
    harness
        .authority
        .validation_pending
        .store(false, Ordering::SeqCst);
    harness.runtime.complete_next();
    harness.session.flush_due(harness.now_ms);
    assert_eq!(harness.pull(Some(&result))["result"]["kind"], "unchanged");
    assert_eq!(harness.runtime.started(), 2);
    assert_eq!(
        harness.authority.generation.load(Ordering::SeqCst),
        1,
        "validation must not execute a new analyzer refresh"
    );
}

#[test]
fn settled_pull_validations_release_capacity_without_client_retries() {
    let mut harness = Harness::with_refresh_support(true, true);
    let mut reports = Vec::new();
    for index in 0..MAX_DIAGNOSTIC_OPERATIONS {
        let uri = format!("file:///root/retained-{index:03}.rs");
        assert_eq!(harness.pull_document(&uri, None)["error"]["code"], -32802);
        harness.runtime.complete_next();
        let result_id = assert_full(&harness.pull_document(&uri, None), index as u64 + 1, 0);
        reports.push((uri, result_id));
    }
    assert_eq!(harness.runtime.started(), reports.len());
    harness
        .authority
        .validation_pending
        .store(true, Ordering::SeqCst);
    for (uri, result_id) in &reports {
        let pending = harness.pull_document(uri, Some(result_id));
        assert_eq!(pending["error"]["code"], -32802, "{pending}");
        assert!(pending["error"]["data"]["operationId"].as_str().is_some());
        assert!(pending.get("result").is_none());
    }
    assert_eq!(harness.runtime.started(), reports.len() * 2);

    let fresh_uri = "file:///root/fresh.rs";
    let saturated = harness.pull_document(fresh_uri, None);
    assert_eq!(saturated["error"]["code"], -32802, "{saturated}");
    assert_eq!(
        saturated["error"]["data"]["failureClass"],
        "diagnostic-capacity"
    );
    assert_eq!(harness.runtime.started(), reports.len() * 2);

    harness
        .authority
        .validation_pending
        .store(false, Ordering::SeqCst);
    for _ in &reports {
        harness.runtime.complete_next();
    }
    harness.session.flush_due(harness.now_ms);
    assert!(
        harness.messages().is_empty(),
        "validation settlement must emit neither diagnostics nor a workspace refresh"
    );

    let admitted = harness.pull_document(fresh_uri, None);
    assert_eq!(admitted["error"]["code"], -32802, "{admitted}");
    assert_eq!(
        admitted["error"]["data"]["coverage"], "refresh-required",
        "{admitted}"
    );
    let started = harness.runtime.started();
    assert_eq!(
        started,
        reports.len() * 2 + 1,
        "settled validations must release capacity without another pull: {admitted}"
    );
    harness
        .authority
        .validation_pending
        .store(true, Ordering::SeqCst);
    let (uri, result_id) = reports.last().unwrap();
    assert_eq!(
        harness.pull_document(uri, Some(result_id))["result"],
        json!({ "kind": "unchanged", "resultId": result_id }),
        "a surviving report must consume its settled validation"
    );
    assert_eq!(harness.runtime.started(), started);
    assert_eq!(
        harness.authority.generation.load(Ordering::SeqCst),
        reports.len() as u64
    );

    harness
        .authority
        .validation_pending
        .store(false, Ordering::SeqCst);
    harness.runtime.complete_next();
    assert_full(
        &harness.pull_document(fresh_uri, None),
        reports.len() as u64 + 1,
        0,
    );
}

#[test]
fn dependency_changes_invalidate_retained_document_diagnostics() {
    for (method, params) in [
        (
            "textDocument/didSave",
            json!({ "textDocument": { "uri": "file:///root/b.rs" } }),
        ),
        (
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": "file:///root/b.rs", "version": 2 },
                "contentChanges": [{ "text": "pub fn changed_dependency() {}" }],
            }),
        ),
    ] {
        let mut harness = Harness::new(false);
        harness.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": "file:///root/b.rs", "languageId": "rust", "version": 1,
                    "text": "pub fn dependency() {}",
                },
            }),
        );
        harness.open(1);
        assert_eq!(harness.pull(None)["error"]["code"], -32802);
        harness.runtime.complete_next();
        let old_result = assert_full(&harness.pull(None), 1, 1);

        harness.notify(method, params);
        let pending = harness.pull(Some(&old_result));
        assert_eq!(pending["error"]["code"], -32802, "{method}: {pending}");
        assert!(pending.get("result").is_none());
        while harness
            .runtime
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|task| task.0.lock().unwrap().is_some())
        {
            harness.runtime.complete_next();
        }
        let generation = harness.authority.generation.load(Ordering::SeqCst);
        let new_result = assert_full(&harness.pull(Some(&old_result)), generation, 1);
        assert_ne!(new_result, old_result);
        let started = harness.runtime.started();
        assert_eq!(
            harness.pull(Some(&new_result))["result"]["kind"],
            "unchanged"
        );
        assert_eq!(harness.runtime.started(), started);
    }
}

#[test]
fn dependency_invalidation_retires_late_work_and_preserves_other_roots() {
    let mut harness = Harness::new(false);
    harness.notify(
        "workspace/didChangeWorkspaceFolders",
        json!({
            "event": {
                "added": [
                    { "uri": "file:///other", "name": "other" },
                    { "uri": "file:///root/nested", "name": "nested" },
                ],
                "removed": [],
            },
        }),
    );
    let mutation = harness.session.pending_workspace_folder_mutation().unwrap();
    harness
        .session
        .apply_workspace_folder_mutation(
            &mutation,
            AuthorizedLspWorkspace::new(
                Some(digest('d')),
                vec![
                    AdmittedRoot::authorized("file:///root", digest('b')),
                    AdmittedRoot::authorized("file:///other", digest('e')),
                    AdmittedRoot::authorized("file:///root/nested", digest('f')),
                ],
            )
            .unwrap(),
        )
        .unwrap();
    let mut unaffected = Vec::new();
    for (index, uri) in ["file:///other/a.rs", "file:///root/nested/a.rs"]
        .into_iter()
        .enumerate()
    {
        assert_eq!(harness.pull_document(uri, None)["error"]["code"], -32802);
        harness.runtime.complete_next();
        let result = assert_full(&harness.pull_document(uri, None), index as u64 + 1, 0);
        unaffected.push((uri, result));
    }
    harness.open(1);
    assert_eq!(harness.pull(None)["error"]["code"], -32802);
    harness.notify(
        "textDocument/didSave",
        json!({
            "textDocument": { "uri": "file:///root/b.rs" },
        }),
    );
    assert_eq!(harness.pull(None)["error"]["code"], -32802);
    // The first A operation finishes after B invalidated its ownership.
    harness.runtime.complete_next();
    let late = harness.pull(None);
    assert_eq!(late["error"]["code"], -32802, "{late}");
    assert!(late.get("result").is_none());
    harness.runtime.complete_next();
    harness.runtime.complete_next();
    assert_full(&harness.pull(None), 5, 1);
    for (uri, result) in unaffected {
        assert_eq!(
            harness.pull_document(uri, Some(&result))["result"]["kind"],
            "unchanged"
        );
    }
    assert_eq!(harness.runtime.started(), 5);
}

#[test]
fn dependency_invalidation_discards_queued_stale_publications() {
    let mut harness = Harness::new(true);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    harness.session.flush_due(harness.now_ms);
    // Leave A's completed publication queued while the dependency changes.
    let messages = harness.notify(
        "textDocument/didSave",
        json!({
            "textDocument": { "uri": "file:///root/b.rs" },
        }),
    );
    assert!(
        !messages.iter().any(|message| {
            message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == DOCUMENT
        }),
        "{messages:?}"
    );
    assert_eq!(harness.pull(None)["error"]["code"], -32802);
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
    // Capacity admission is independent of time; all messages share one tick.
    harness.message_duration_ms = 0;
    for index in 0..MAX_WORKSPACE_DIAGNOSTIC_RESULTS {
        // Admit all overlays before computing reports: opening another file
        // now correctly invalidates dependent reports in the same root.
        let uri = format!("file:///root/open-{index}.rs");
        harness.notify("textDocument/didOpen", json!({
            "textDocument": { "uri": uri, "languageId": "rust", "version": 1, "text": "fn a() {}" },
        }));
    }
    for index in 0..MAX_WORKSPACE_DIAGNOSTIC_RESULTS {
        let uri = format!("file:///root/open-{index}.rs");
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

fn fill_outbound_queue(harness: &mut Harness) -> usize {
    let mut queued = 0;
    for _ in 0..MAX_QUEUED_OUTBOUND_MESSAGES {
        if !harness.session.enqueue_value(json!({
            "jsonrpc": "2.0", "method": "window/logMessage",
            "params": { "type": 3, "message": "transport occupied" },
        })) {
            break;
        }
        queued += 1;
    }
    assert!(
        !harness
            .session
            .has_outbound_capacity(crate::MAX_PUBLICATION_BYTES)
    );
    queued
}

fn validation_harness() -> (Harness, String) {
    let mut harness = Harness::with_refresh_support(true, true);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    let messages = harness.send(json!({
        "jsonrpc": "2.0", "id": "initial-report", "method": "textDocument/diagnostic",
        "params": { "textDocument": { "uri": DOCUMENT } },
    }));
    let report = messages
        .iter()
        .find(|message| message["id"] == "initial-report")
        .unwrap();
    let result_id = assert_full(report, 1, 1);
    let refresh = messages
        .iter()
        .find(|message| message["method"] == "workspace/diagnostic/refresh")
        .unwrap();
    assert_eq!(
        harness.send(json!({
            "jsonrpc": "2.0", "id": refresh["id"], "result": null,
        })),
        Vec::<Value>::new()
    );
    (harness, result_id)
}

fn assert_settled_validation_failure(failure: Option<&'static str>, background: bool) {
    let (mut harness, result_id) = validation_harness();
    harness
        .authority
        .validation_pending
        .store(true, Ordering::SeqCst);
    assert_eq!(harness.pull(Some(&result_id))["error"]["code"], -32802);
    assert_eq!(harness.runtime.started(), 2);

    *harness.authority.validation_failure.lock().unwrap() = failure;
    if failure.is_none() {
        *harness.authority.authority_digest.lock().unwrap() = Some(digest('b'));
    }
    harness
        .authority
        .validation_pending
        .store(false, Ordering::SeqCst);
    harness.runtime.complete_next();
    if background {
        let queued = fill_outbound_queue(&mut harness);
        harness.session.flush_due(harness.now_ms);
        let messages = harness.messages();
        assert_eq!(messages.len(), queued);
        assert!(
            messages
                .iter()
                .all(|message| message["method"] == "window/logMessage")
        );
        assert!(harness.session.diagnostics.active_refreshes.is_empty());
    }

    let messages = harness.send(json!({
        "jsonrpc": "2.0", "id": "validation-result", "method": "textDocument/diagnostic",
        "params": { "textDocument": { "uri": DOCUMENT }, "previousResultId": result_id },
    }));
    assert_eq!(
        messages.len(),
        1,
        "validation consumption must emit only its response: {messages:?}"
    );
    let failed = &messages[0];
    assert_eq!(failed["id"], "validation-result");
    assert_eq!(failed["error"]["code"], -32802, "{failed}");
    assert!(failed.get("result").is_none());
    match failure {
        Some(failure) => assert_eq!(failed["error"]["data"]["failureClass"], failure),
        None => assert_eq!(failed["error"]["data"]["coverage"], "refresh-required"),
    }
    assert_eq!(
        harness.runtime.started(),
        2,
        "consuming a terminal validation must not admit refresh work"
    );
    assert!(harness.session.diagnostics.active_refreshes.is_empty());
    assert!(harness.session.diagnostics.document_snapshots.is_empty());
    assert_eq!(harness.authority.generation.load(Ordering::SeqCst), 1);

    let retry = harness.pull(Some(&result_id));
    assert_eq!(
        retry["error"]["data"]["coverage"], "refresh-required",
        "{retry}"
    );
    assert_eq!(harness.runtime.started(), 3);
    harness.runtime.complete_next();
    assert_full(&harness.pull(Some(&result_id)), 2, 1);
}

#[test]
fn partial_pull_validation_settles_under_backpressure_and_is_consumed_once() {
    assert_settled_validation_failure(None, true);
}

#[test]
fn failed_pull_validation_settles_under_backpressure_and_is_consumed_once() {
    assert_settled_validation_failure(Some("provider-unavailable"), true);
}

#[test]
fn directly_consumed_failed_validation_does_not_admit_a_refresh() {
    assert_settled_validation_failure(Some("provider-unavailable"), false);
}

#[test]
fn settled_ready_validation_is_consumed_once_and_preserves_the_generation_floor() {
    let (mut harness, result_id) = validation_harness();
    harness
        .authority
        .validation_pending
        .store(true, Ordering::SeqCst);
    assert_eq!(harness.pull(Some(&result_id))["error"]["code"], -32802);
    harness
        .authority
        .validation_pending
        .store(false, Ordering::SeqCst);
    harness.runtime.complete_next();
    let queued = fill_outbound_queue(&mut harness);
    harness.session.flush_due(harness.now_ms);
    let messages = harness.messages();
    assert_eq!(messages.len(), queued);
    assert!(
        messages
            .iter()
            .all(|message| message["method"] == "window/logMessage")
    );
    assert!(harness.session.diagnostics.active_refreshes.is_empty());

    harness
        .authority
        .validation_pending
        .store(true, Ordering::SeqCst);
    assert_eq!(
        harness.pull(Some(&result_id))["result"]["kind"],
        "unchanged"
    );
    assert_eq!(harness.runtime.started(), 2);
    let validating = harness.pull(Some(&result_id));
    assert_eq!(validating["error"]["code"], -32802, "{validating}");
    assert!(
        validating["error"]["data"]["operationId"]
            .as_str()
            .is_some()
    );
    assert_eq!(harness.runtime.started(), 3);
    harness
        .authority
        .validation_pending
        .store(false, Ordering::SeqCst);
    harness.runtime.complete_next();
    harness.session.flush_due(harness.now_ms);
    assert_eq!(harness.messages(), Vec::<Value>::new());

    *harness.authority.next_generation.lock().unwrap() = Some(0);
    harness.notify(
        "textDocument/didSave",
        json!({ "textDocument": { "uri": DOCUMENT } }),
    );
    harness.runtime.complete_next();
    let stale = harness.pull(Some(&result_id));
    assert_eq!(
        stale["error"]["data"]["failureClass"], "superseded-generation",
        "{stale}"
    );
    assert!(stale.get("result").is_none());
}

#[test]
fn publication_retry_uses_the_admitted_snapshot_without_polling_or_revalidation() {
    let mut harness = Harness::new(true);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    fill_outbound_queue(&mut harness);
    harness.session.poll_diagnostic_refresh(DOCUMENT);
    harness.messages();
    let reads = harness
        .session
        .diagnostics
        .provider
        .reads
        .load(Ordering::SeqCst);
    harness
        .authority
        .validation_pending
        .store(true, Ordering::SeqCst);
    harness.session.flush_due(harness.now_ms);
    let messages = harness.messages();
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0]["method"], "textDocument/publishDiagnostics");
    assert_eq!(messages[0]["params"]["diagnostics"], expected_items(1, 1));
    assert_eq!(
        harness
            .session
            .diagnostics
            .provider
            .reads
            .load(Ordering::SeqCst),
        reads
    );
    assert_eq!(harness.runtime.started(), 1);
    assert_completed_refresh_is_not_polled_again(&mut harness);
}

#[test]
fn late_validation_cannot_answer_a_reopened_document_or_retire_its_refresh() {
    let mut harness = Harness::new(false);
    harness.open(1);
    harness.start_refresh();
    harness.runtime.complete_next();
    let result_id = assert_full(&harness.pull(None), 1, 1);
    harness
        .authority
        .validation_pending
        .store(true, Ordering::SeqCst);
    assert_eq!(harness.pull(Some(&result_id))["error"]["code"], -32802);
    harness.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": DOCUMENT } }),
    );
    harness.open(1);
    assert_eq!(harness.pull(Some(&result_id))["error"]["code"], -32802);
    assert_eq!(harness.runtime.started(), 3);
    harness
        .authority
        .validation_pending
        .store(false, Ordering::SeqCst);
    harness.runtime.complete_next();
    harness.session.flush_due(harness.now_ms);
    assert_eq!(harness.messages(), Vec::<Value>::new());
    let pending = harness.pull(Some(&result_id));
    assert_eq!(pending["error"]["code"], -32802, "{pending}");
    assert!(pending["error"]["data"]["operationId"].as_str().is_some());
    assert_eq!(harness.runtime.started(), 3);
    harness.runtime.complete_next();
    assert_full(&harness.pull(Some(&result_id)), 2, 1);
}
