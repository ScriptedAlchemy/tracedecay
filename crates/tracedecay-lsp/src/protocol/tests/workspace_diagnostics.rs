use super::*;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};

use crate::gateway::{LspRuntimeFailure, LspRuntimeFuture, LspRuntimeSpawner, LspRuntimeTask};
use crate::overlay::{
    CanonicalDiagnosticRefreshRequest, CanonicalDiagnosticSnapshotAuthority,
    DiagnosticSnapshotAdapter,
};
use crate::workspace_diagnostics::{
    CanonicalWorkspaceDiagnosticRefreshRequest, MAX_WORKSPACE_DIAGNOSTIC_FANOUT,
};
use tracedecay_domain::test_fixtures::digest;

fn initialize_workspace<D>(
    roots: Vec<AdmittedRoot>,
    diagnostics: D,
) -> DaemonLspProtocolSession<Feedback, Semantics, D>
where
    D: DiagnosticSnapshotPort,
{
    let workspace = AuthorizedLspWorkspace::new(Some(digest('c')), roots.clone()).unwrap();
    let gateway_capabilities = GatewayCapabilities {
        supports_workspace_folders: true,
        supports_workspace_diagnostics: true,
        ..GatewayCapabilities::default()
    };
    let upstream = UpstreamCapabilities {
        supports_diagnostics: true,
        semantic: BTreeSet::new(),
    };
    let initial = negotiate_capabilities(
        &ClientCapabilities::default(),
        &gateway_capabilities,
        &upstream,
    );
    let mut session = DaemonLspProtocolSession::from_workspace_ports(
        workspace,
        initial,
        gateway_capabilities,
        upstream,
        Feedback::default(),
        Semantics,
        diagnostics,
    );
    let folders = roots
        .iter()
        .map(|root| json!({ "uri": root.uri(), "name": root.uri() }))
        .collect::<Vec<_>>();
    session.handle_payload(
        &serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "workspaceFolders": folders,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": { "diagnostic": {} },
                    "workspace": { "workspaceFolders": true },
                },
            },
        }))
        .unwrap(),
        0,
    );
    session.drain_outbound();
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    session
}

#[test]
fn workspace_diagnostics_preserve_ready_roots_when_one_root_fails() {
    let roots = vec![
        AdmittedRoot::authorized("file:///left", digest('a')),
        AdmittedRoot::authorized("file:///failed", digest('b')),
    ];
    let mut session = initialize_workspace(roots, Diagnostics);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        2,
    );
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(response["result"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(response["result"]["tracedecay"]["complete"], false);
    assert_eq!(
        response["result"]["tracedecay"]["rootFailures"][0]["rootUri"],
        "file:///failed"
    );
    assert_eq!(
        response["result"]["tracedecay"]["rootFailures"][0]["failureClass"],
        "indexed-generation-unavailable"
    );
}

#[test]
fn nested_workspace_diagnostics_route_documents_to_the_deepest_root() {
    let mut session = initialize_workspace(
        vec![
            AdmittedRoot::authorized("file:///workspace", digest('a')),
            AdmittedRoot::authorized("file:///workspace/nested", digest('b')),
        ],
        Diagnostics,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        2,
    );
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    let uris = response["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["uri"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        uris,
        BTreeSet::from([
            "file:///workspace/src/lib.rs",
            "file:///workspace/nested/src/lib.rs",
        ])
    );
}

#[derive(Clone)]
struct MutableWorkspaceDiagnostics {
    message: Arc<Mutex<String>>,
    authority: Arc<Mutex<char>>,
}

impl DiagnosticSnapshotPort for MutableWorkspaceDiagnostics {
    fn document_diagnostics(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        DiagnosticSnapshotOutcome::Failed {
            source_generation: None,
            failure_class: "document-diagnostics-not-used".to_owned(),
        }
    }

    fn supports_workspace_diagnostics(&self) -> bool {
        true
    }

    fn workspace_diagnostics(
        &self,
        workspace: &AuthorizedLspWorkspace,
        root: &AdmittedRoot,
        _overlays: &[OverlaySnapshot],
    ) -> WorkspaceDiagnosticSnapshotOutcome {
        let uri = format!("{}/src/lib.rs", root.uri());
        assert_eq!(workspace.resolve_document(&uri), Ok(root));
        WorkspaceDiagnosticSnapshotOutcome::Ready {
            diagnostics: WorkspaceGenerationDiagnostics {
                code_generation_id: "code-generation-7".to_owned(),
                snapshot_digest: digest('d'),
                documents: vec![WorkspaceDocumentDiagnostics {
                    uri: uri.clone(),
                    version: None,
                    content_digest: ContentDigest::of_bytes(b"same-source"),
                    diagnostics: GenerationDiagnostics {
                        generation: 7,
                        authority_digest: digest(*self.authority.lock().unwrap()),
                        upstream: vec![GatewayDiagnostic {
                            uri,
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
                            code: Some("mutable".to_owned()),
                            code_description_uri: None,
                            message: self.message.lock().unwrap().clone(),
                            source: DiagnosticSource::Upstream,
                            related_information: Vec::new(),
                            data: None,
                        }],
                        tracedecay: Vec::new(),
                    },
                }],
            },
            completed_operation_id: None,
        }
    }
}

#[test]
fn previous_result_id_changes_when_merged_diagnostic_contents_change() {
    let message = Arc::new(Mutex::new("first".to_owned()));
    let authority = Arc::new(Mutex::new('e'));
    let diagnostics = MutableWorkspaceDiagnostics {
        message: Arc::clone(&message),
        authority,
    };
    let mut session = initialize_workspace(
        vec![AdmittedRoot::authorized("file:///root", digest('a'))],
        diagnostics,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        2,
    );
    let first: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    let first_item = &first["result"]["items"][0];
    let first_result_id = first_item["resultId"].as_str().unwrap().to_owned();
    *message.lock().unwrap() = "second".to_owned();

    session.handle_payload(
        &serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "workspace/diagnostic",
            "params": {
                "previousResultIds": [{
                    "uri": first_item["uri"],
                    "value": first_result_id,
                }],
            },
        }))
        .unwrap(),
        3,
    );
    let second: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(second["result"]["items"][0]["kind"], "full");
    assert_ne!(
        second["result"]["items"][0]["resultId"],
        first_item["resultId"]
    );
    assert_eq!(
        second["result"]["items"][0]["items"][0]["message"],
        "second"
    );
}

#[test]
fn previous_result_id_changes_when_diagnostic_authority_changes() {
    let message = Arc::new(Mutex::new("stable".to_owned()));
    let authority = Arc::new(Mutex::new('e'));
    let diagnostics = MutableWorkspaceDiagnostics {
        message,
        authority: Arc::clone(&authority),
    };
    let mut session = initialize_workspace(
        vec![AdmittedRoot::authorized("file:///root", digest('a'))],
        diagnostics,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        2,
    );
    let first: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    let first_item = &first["result"]["items"][0];
    let first_result_id = first_item["resultId"].as_str().unwrap().to_owned();
    *authority.lock().unwrap() = 'f';

    session.handle_payload(
        &serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "workspace/diagnostic",
            "params": {
                "previousResultIds": [{
                    "uri": first_item["uri"],
                    "value": first_result_id,
                }],
            },
        }))
        .unwrap(),
        3,
    );
    let second: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(second["result"]["items"][0]["kind"], "full");
    assert_ne!(
        second["result"]["items"][0]["resultId"],
        first_item["resultId"]
    );
    assert_eq!(
        second["result"]["items"][0]["items"][0]["message"],
        "stable"
    );
}

#[derive(Clone)]
struct ToggleDynamicWorkspaceDiagnostics {
    ready: Arc<AtomicBool>,
}

impl DiagnosticSnapshotPort for ToggleDynamicWorkspaceDiagnostics {
    fn document_diagnostics(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        Diagnostics.document_diagnostics(root, document_uri, overlay)
    }

    fn supports_workspace_diagnostics(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    fn workspace_diagnostics(
        &self,
        workspace: &AuthorizedLspWorkspace,
        root: &AdmittedRoot,
        overlays: &[OverlaySnapshot],
    ) -> WorkspaceDiagnosticSnapshotOutcome {
        if self.supports_workspace_diagnostics() {
            Diagnostics.workspace_diagnostics(workspace, root, overlays)
        } else {
            WorkspaceDiagnosticSnapshotOutcome::Failed {
                code_generation_id: None,
                failure_class: "workspace-diagnostics-unavailable".to_owned(),
            }
        }
    }
}

fn dynamic_workspace_session(
    ready: Arc<AtomicBool>,
) -> DaemonLspProtocolSession<Feedback, Semantics, ToggleDynamicWorkspaceDiagnostics> {
    let root = AdmittedRoot::authorized("file:///root", digest('a'));
    let workspace = AuthorizedLspWorkspace::new(Some(digest('c')), vec![root]).unwrap();
    let gateway_capabilities = GatewayCapabilities {
        supports_document_diagnostics: true,
        supports_managed_diagnostics: true,
        supports_workspace_diagnostics: true,
        ..GatewayCapabilities::default()
    };
    let upstream = UpstreamCapabilities::default();
    let initial = negotiate_capabilities(
        &ClientCapabilities::default(),
        &gateway_capabilities,
        &upstream,
    );
    let mut session = DaemonLspProtocolSession::from_workspace_ports(
        workspace,
        initial,
        gateway_capabilities,
        upstream,
        Feedback::default(),
        Semantics,
        ToggleDynamicWorkspaceDiagnostics { ready },
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":["utf-16"]},"textDocument":{"diagnostic":{"dynamicRegistration":true}},"workspace":{"diagnostic":{"refreshSupport":true}}}}}"#,
        0,
    );
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert!(
        response["result"]["capabilities"]
            .get("diagnosticProvider")
            .is_none()
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    session
}

fn dynamic_request_id(frame: &Value) -> String {
    frame["id"].as_str().unwrap().to_owned()
}

fn acknowledge_dynamic_request<P, S, D>(
    session: &mut DaemonLspProtocolSession<P, S, D>,
    request_id: &str,
    now_ms: u64,
) where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    session.handle_payload(
        &serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": null,
        }))
        .unwrap(),
        now_ms,
    );
}

#[test]
fn initialized_client_registers_and_unregisters_workspace_diagnostics_with_live_readiness() {
    let ready = Arc::new(AtomicBool::new(false));
    let mut session = dynamic_workspace_session(Arc::clone(&ready));
    assert_eq!(session.drain_outbound().len(), 0);

    ready.store(true, Ordering::Release);
    session.flush_due(2);
    let registered = session.drain_outbound();
    assert_eq!(registered.len(), 1);
    let register: Value = serde_json::from_slice(&registered[0]).unwrap();
    assert_eq!(register["method"], "client/registerCapability");
    assert_eq!(
        register["params"]["registrations"][0]["method"],
        "textDocument/diagnostic"
    );
    assert_eq!(
        register["params"]["registrations"][0]["registerOptions"]["workspaceDiagnostics"],
        true
    );
    session.flush_due(3);
    assert_eq!(session.drain_outbound().len(), 0);

    acknowledge_dynamic_request(&mut session, &dynamic_request_id(&register), 4);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        5,
    );
    let available: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert!(available.get("result").is_some());

    ready.store(false, Ordering::Release);
    session.flush_due(6);
    let unregistered = session.drain_outbound();
    assert_eq!(unregistered.len(), 1);
    let unregister: Value = serde_json::from_slice(&unregistered[0]).unwrap();
    assert_eq!(unregister["method"], "client/unregisterCapability");
    assert_eq!(
        unregister["params"]["unregisterations"][0]["id"],
        "tracedecay.workspace-diagnostics.v1"
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":3,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        7,
    );
    let unavailable: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(unavailable["error"]["code"], -32601);

    acknowledge_dynamic_request(&mut session, &dynamic_request_id(&unregister), 8);
    ready.store(true, Ordering::Release);
    session.flush_due(9);
    let restarted: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(restarted["method"], "client/registerCapability");
}

#[test]
fn readiness_loss_cancels_an_unacknowledged_registration_before_unregistration() {
    let ready = Arc::new(AtomicBool::new(true));
    let mut session = dynamic_workspace_session(Arc::clone(&ready));
    let register: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();

    ready.store(false, Ordering::Release);
    session.flush_due(2);
    let frames = session
        .drain_outbound()
        .into_iter()
        .map(|frame| serde_json::from_slice::<Value>(&frame).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["method"], "$/cancelRequest");
    assert_eq!(frames[0]["params"]["id"], register["id"]);
    assert_eq!(frames[1]["method"], "client/unregisterCapability");
    session.flush_due(3);
    assert_eq!(session.drain_outbound().len(), 0);
}

#[test]
fn reconnect_resynchronizes_dynamic_registration_without_requiring_client_restart() {
    let ready = Arc::new(AtomicBool::new(true));
    let mut session = dynamic_workspace_session(ready);
    let register: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    acknowledge_dynamic_request(&mut session, &dynamic_request_id(&register), 2);

    session.detach().unwrap();
    session.reconnect().unwrap();
    let unregister: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(unregister["method"], "client/unregisterCapability");
    acknowledge_dynamic_request(&mut session, &dynamic_request_id(&unregister), 3);
    let reregister: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(reregister["method"], "client/registerCapability");
}

#[test]
fn rejected_registration_stays_unavailable_until_a_new_readiness_epoch() {
    let ready = Arc::new(AtomicBool::new(true));
    let mut session = dynamic_workspace_session(Arc::clone(&ready));
    let register: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    session.handle_payload(
        &serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": dynamic_request_id(&register),
            "error": { "code": -32603, "message": "registration rejected" },
        }))
        .unwrap(),
        2,
    );
    session.flush_due(3);
    assert_eq!(session.drain_outbound().len(), 0);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        4,
    );
    let unavailable: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(unavailable["error"]["code"], -32601);

    ready.store(false, Ordering::Release);
    session.flush_due(5);
    ready.store(true, Ordering::Release);
    session.flush_due(6);
    let retry: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(retry["method"], "client/registerCapability");
    assert_ne!(retry["id"], register["id"]);
}

fn fill_ordinary_outbound_capacity<P, S, D>(session: &mut DaemonLspProtocolSession<P, S, D>)
where
    P: FeedbackCyclePort,
    S: SemanticProviderPort,
    D: DiagnosticSnapshotPort,
{
    for ordinal in 0..MAX_QUEUED_OUTBOUND_MESSAGES {
        if !session.enqueue_value(json!({
            "jsonrpc": "2.0",
            "method": "window/logMessage",
            "params": { "type": 4, "message": format!("queued-{ordinal}") },
        })) {
            return;
        }
    }
    panic!("ordinary messages consumed the reserved capability-control capacity");
}

#[test]
fn full_ordinary_queue_cannot_starve_readiness_loss_controls() {
    let ready = Arc::new(AtomicBool::new(true));
    let mut session = dynamic_workspace_session(Arc::clone(&ready));
    let register: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    fill_ordinary_outbound_capacity(&mut session);

    ready.store(false, Ordering::Release);
    session.flush_due(2);
    let controls = session
        .drain_outbound()
        .into_iter()
        .filter_map(|frame| serde_json::from_slice::<Value>(&frame).ok())
        .filter(|frame| {
            matches!(
                frame["method"].as_str(),
                Some("$/cancelRequest" | "client/unregisterCapability")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(controls.len(), 2);
    assert_eq!(controls[0]["params"]["id"], register["id"]);
    assert_eq!(controls[1]["method"], "client/unregisterCapability");
}

#[test]
fn saturated_reconnect_resync_stays_fail_closed_after_control_enqueue_failure() {
    let ready = Arc::new(AtomicBool::new(true));
    let mut session = dynamic_workspace_session(ready);
    let register: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    acknowledge_dynamic_request(&mut session, &dynamic_request_id(&register), 2);
    fill_ordinary_outbound_capacity(&mut session);

    // The first reset queues unregister. The second consumes the final two
    // reserved slots with cancellation + replacement unregister. The third
    // must retain a fail-closed retry state when no control slot remains.
    for _ in 0..3 {
        session.detach().unwrap();
        session.reconnect().unwrap();
    }
    session.drain_outbound();
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":9,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        3,
    );
    let unavailable: Value = session
        .drain_outbound()
        .into_iter()
        .filter_map(|frame| serde_json::from_slice::<Value>(&frame).ok())
        .find(|frame| frame["id"] == 9)
        .unwrap();
    assert_eq!(unavailable["error"]["code"], -32601);
}

#[derive(Clone)]
struct ChangingPartialWorkspaceDiagnostics {
    phase: Arc<AtomicU8>,
    left_reads: Arc<AtomicUsize>,
}

impl ChangingPartialWorkspaceDiagnostics {
    fn ready_snapshot(
        root: &AdmittedRoot,
        message: &str,
        generation: u64,
    ) -> WorkspaceDiagnosticSnapshotOutcome {
        let uri = format!("{}/src/lib.rs", root.uri());
        WorkspaceDiagnosticSnapshotOutcome::Ready {
            diagnostics: WorkspaceGenerationDiagnostics {
                code_generation_id: format!("code-generation-{generation}"),
                snapshot_digest: digest(if generation == 1 { 'd' } else { 'e' }),
                documents: vec![WorkspaceDocumentDiagnostics {
                    uri: uri.clone(),
                    version: None,
                    content_digest: ContentDigest::of_bytes(message.as_bytes()),
                    diagnostics: GenerationDiagnostics {
                        generation,
                        authority_digest: digest(if generation == 1 { 'f' } else { '9' }),
                        upstream: vec![GatewayDiagnostic {
                            uri,
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
                            code: Some("generation".to_owned()),
                            code_description_uri: None,
                            message: message.to_owned(),
                            source: DiagnosticSource::Upstream,
                            related_information: Vec::new(),
                            data: None,
                        }],
                        tracedecay: Vec::new(),
                    },
                }],
            },
            completed_operation_id: None,
        }
    }
}

impl DiagnosticSnapshotPort for ChangingPartialWorkspaceDiagnostics {
    fn document_diagnostics(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        DiagnosticSnapshotOutcome::Failed {
            source_generation: None,
            failure_class: "not-used".to_owned(),
        }
    }

    fn supports_workspace_diagnostics(&self) -> bool {
        true
    }

    fn workspace_diagnostics(
        &self,
        _workspace: &AuthorizedLspWorkspace,
        root: &AdmittedRoot,
        _overlays: &[OverlaySnapshot],
    ) -> WorkspaceDiagnosticSnapshotOutcome {
        let phase = self.phase.load(Ordering::Acquire);
        if root.uri() == "file:///left" {
            self.left_reads.fetch_add(1, Ordering::AcqRel);
            return Self::ready_snapshot(
                root,
                if phase == 1 { "old-left" } else { "new-left" },
                u64::from(phase),
            );
        }
        if phase == 1 {
            WorkspaceDiagnosticSnapshotOutcome::Refreshing(DiagnosticRefreshIdentity {
                operation_id: "refresh-right".to_owned(),
                source_generation: Some(1),
                target_generation: Some(2),
            })
        } else {
            Self::ready_snapshot(root, "new-right", 2)
        }
    }
}

#[test]
fn partial_workspace_retry_replaces_roots_from_the_changed_generation() {
    let phase = Arc::new(AtomicU8::new(1));
    let left_reads = Arc::new(AtomicUsize::new(0));
    let diagnostics = ChangingPartialWorkspaceDiagnostics {
        phase: Arc::clone(&phase),
        left_reads: Arc::clone(&left_reads),
    };
    let mut session = initialize_workspace(
        vec![
            AdmittedRoot::authorized("file:///left", digest('a')),
            AdmittedRoot::authorized("file:///right", digest('b')),
        ],
        diagnostics,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        2,
    );
    let pending: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(pending["error"]["code"], -32802);

    phase.store(2, Ordering::Release);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":3,"method":"workspace/diagnostic","params":{"previousResultIds":[]}}"#,
        3,
    );
    let completed: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(left_reads.load(Ordering::Acquire), 2);
    let messages = completed["result"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|item| item["items"].as_array().into_iter().flatten())
        .filter_map(|item| item["message"].as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(messages, BTreeSet::from(["new-left", "new-right"]));
}

#[derive(Default)]
struct ControlledWorkspaceRuntime(Mutex<Vec<Arc<ControlledWorkspaceTask>>>);

struct ControlledWorkspaceTask {
    future: Mutex<Option<LspRuntimeFuture<()>>>,
    aborted: AtomicBool,
}

impl LspRuntimeTask for Arc<ControlledWorkspaceTask> {
    fn abort(&self) {
        // Aborting result ownership does not settle accepted analyzer work.
        // Tests can finish it later and observe that its result is discarded.
        self.aborted.store(true, Ordering::Release);
    }
}

impl LspRuntimeSpawner for ControlledWorkspaceRuntime {
    fn spawn(&self, future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
        let task = Arc::new(ControlledWorkspaceTask {
            future: Mutex::new(Some(future)),
            aborted: AtomicBool::new(false),
        });
        self.0.lock().unwrap().push(Arc::clone(&task));
        Box::new(task)
    }
}

impl ControlledWorkspaceRuntime {
    fn complete(&self, index: usize) {
        let task = Arc::clone(&self.0.lock().unwrap()[index]);
        let mut future = task.future.lock().unwrap().take().unwrap();
        assert_eq!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Ready(())
        );
    }

    fn started(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    fn aborted(&self, index: usize) -> bool {
        self.0.lock().unwrap()[index]
            .aborted
            .load(Ordering::Acquire)
    }
}

struct WorkspaceAuthority;

impl CanonicalDiagnosticSnapshotAuthority for WorkspaceAuthority {
    fn refresh(
        &self,
        _request: CanonicalDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<GenerationDiagnostics, LspRuntimeFailure>> {
        Box::pin(async { Err(LspRuntimeFailure::new("document-refresh-finished")) })
    }

    fn supports_workspace_diagnostics(&self) -> bool {
        true
    }

    fn refresh_workspace(
        &self,
        _request: CanonicalWorkspaceDiagnosticRefreshRequest,
    ) -> LspRuntimeFuture<Result<WorkspaceGenerationDiagnostics, LspRuntimeFailure>> {
        Box::pin(async {
            Ok(WorkspaceGenerationDiagnostics {
                code_generation_id: "current-generation".to_owned(),
                snapshot_digest: digest('f'),
                documents: Vec::new(),
            })
        })
    }
}

#[test]
fn workspace_root_replacement_releases_abandoned_operation_capacity() {
    let runtime = Arc::new(ControlledWorkspaceRuntime::default());
    let adapter = Arc::new(DiagnosticSnapshotAdapter::new(
        runtime.clone(),
        Arc::new(WorkspaceAuthority),
    ));
    let primary = AdmittedRoot::authorized("file:///primary", digest('a'));
    let mut secondary = AdmittedRoot::authorized("file:///secondary-0", digest('b'));
    let mut session = initialize_workspace(
        vec![primary.clone(), secondary.clone()],
        Arc::clone(&adapter),
    );

    for ordinal in 0..MAX_WORKSPACE_DIAGNOSTIC_FANOUT * 2 {
        let first_task = runtime.started();
        let pending = session.pull_workspace_diagnostics(&json!({})).unwrap_err();
        assert_eq!(pending.data["coverage"], "workspace-roots-ready=0/2");
        assert_eq!(runtime.started(), first_task + 2);
        assert_eq!(session.diagnostics.workspace_refreshes.len(), 2);

        let next =
            AdmittedRoot::authorized(format!("file:///secondary-{}", ordinal + 1), digest('b'));
        session
            .handle_workspace_folders_changed(&json!({ "event": {
                "added": [{ "uri": next.uri() }],
                "removed": [{ "uri": secondary.uri() }],
            }}))
            .unwrap();
        let mutation = session.pending_workspace_folder_mutation().unwrap();
        session
            .apply_workspace_folder_mutation(
                &mutation,
                AuthorizedLspWorkspace::new(
                    Some(digest(if ordinal % 2 == 0 { 'd' } else { 'c' })),
                    vec![primary.clone(), next.clone()],
                )
                .unwrap(),
            )
            .unwrap();
        assert!(session.diagnostics.workspace_refreshes.is_empty());
        for task in first_task..first_task + 2 {
            assert!(runtime.aborted(task));
            runtime.complete(task);
        }
        assert!(matches!(
            adapter.workspace_diagnostics(session.workspace(), &secondary, &[]),
            WorkspaceDiagnosticSnapshotOutcome::Partial { .. }
        ));
        secondary = next;
    }
}

#[test]
fn workspace_overlay_retirement_is_exact_and_discards_late_completion() {
    let runtime = Arc::new(ControlledWorkspaceRuntime::default());
    let adapter = Arc::new(DiagnosticSnapshotAdapter::new(
        runtime.clone(),
        Arc::new(WorkspaceAuthority),
    ));
    let root = AdmittedRoot::authorized("file:///root", digest('a'));
    let mut session = initialize_workspace(vec![root.clone()], Arc::clone(&adapter));
    let uri = "file:///root/lib.rs";
    session
        .handle_did_open(
            &json!({ "textDocument": {
                "uri": uri, "languageId": "rust", "version": 1, "text": "fn first() {}"
            }}),
            2,
        )
        .unwrap();
    assert!(session.pull_workspace_diagnostics(&json!({})).is_err());
    let old_identity = session.diagnostics.workspace_refreshes[root.uri()]
        .identity
        .clone();
    assert!(session.pull_workspace_diagnostics(&json!({})).is_err());
    assert_eq!(runtime.started(), 1, "unchanged refreshes must coalesce");

    session
        .handle_did_change(
            &json!({
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": "fn second() {}" }],
            }),
            3,
        )
        .unwrap();
    assert!(runtime.aborted(0));
    assert!(session.diagnostics.workspace_refreshes.is_empty());
    assert!(session.pull_workspace_diagnostics(&json!({})).is_err());
    let current = session.diagnostics.workspace_refreshes[root.uri()]
        .identity
        .clone();
    assert_ne!(old_identity.operation_id, current.operation_id);
    assert!(!adapter.retire_workspace_refresh(&root, &old_identity.operation_id));
    assert!(!adapter.retire_workspace_refresh(
        &AdmittedRoot::authorized(root.uri(), digest('b')),
        &current.operation_id,
    ));

    runtime.complete(0);
    let overlays = session
        .overlays()
        .snapshots_for_root(session.workspace(), &root);
    assert_eq!(
        adapter.workspace_diagnostics(session.workspace(), &root, &overlays),
        WorkspaceDiagnosticSnapshotOutcome::Refreshing(current),
    );
    assert!(!runtime.aborted(1));
    runtime.complete(1);
    assert_eq!(
        session.pull_workspace_diagnostics(&json!({})).unwrap()["tracedecay"]["complete"],
        true
    );
    assert!(session.diagnostics.workspace_refreshes.is_empty());

    assert!(session.pull_workspace_diagnostics(&json!({})).is_err());
    session
        .handle_did_close(&json!({ "textDocument": { "uri": uri } }), 4)
        .unwrap();
    assert!(runtime.aborted(2));
    runtime.complete(2);
    assert!(matches!(
        adapter.workspace_diagnostics(session.workspace(), &root, &[]),
        WorkspaceDiagnosticSnapshotOutcome::Partial { .. }
    ));
}

#[test]
fn ending_a_session_retires_operations_even_when_the_provider_outlives_it() {
    let root = AdmittedRoot::authorized("file:///root", digest('a'));
    for ending in ["shutdown", "expire", "drop"] {
        let runtime = Arc::new(ControlledWorkspaceRuntime::default());
        let adapter = Arc::new(DiagnosticSnapshotAdapter::new(
            runtime.clone(),
            Arc::new(WorkspaceAuthority),
        ));
        let mut session = initialize_workspace(vec![root.clone()], Arc::clone(&adapter));
        let workspace = session.workspace().clone();
        assert!(session.pull_workspace_diagnostics(&json!({})).is_err());
        assert_eq!(
            session.request_diagnostic_refresh("file:///root/lib.rs", None, None),
            None
        );
        assert_eq!(runtime.started(), 2);
        match ending {
            "shutdown" => session.handle_shutdown_request(json!(2)),
            "expire" => session.expire(),
            "drop" => drop(session),
            _ => unreachable!(),
        }
        for task in 0..2 {
            assert!(
                runtime.aborted(task),
                "{ending} must retire operation {task}"
            );
            runtime.complete(task);
        }
        assert!(matches!(
            adapter.workspace_diagnostics(&workspace, &root, &[]),
            WorkspaceDiagnosticSnapshotOutcome::Partial { .. }
        ));
        assert!(matches!(
            adapter.document_diagnostics(&root, "file:///root/lib.rs", None),
            DiagnosticSnapshotOutcome::Partial { .. }
        ));
        assert!(matches!(
            adapter.request_workspace_refresh(&workspace, &root, &[]),
            DiagnosticRefreshAdmission::Started(_)
        ));
    }
}
