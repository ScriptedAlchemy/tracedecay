use super::context_controller::ContextProjectionCurrentness;
use super::*;
use crate::TRACEDECAY_CONTEXT_REVISION;
use crate::capabilities::SemanticCapability;
use crate::diagnostics::{DiagnosticSeverity, DiagnosticSource, LspPosition, LspRange};
use crate::gateway::{FeedbackCycleRequest, LspLocation, SemanticProviderOutcome, WorkspaceSymbol};
use crate::overlay::OverlaySnapshot;
use crate::provider::GenerationDiagnostics;
use std::cell::RefCell;
use std::sync::Mutex;
use tracedecay_domain::ManifestDigest;

mod diagnostic_publication;

#[derive(Default)]
pub(super) struct Feedback(RefCell<Vec<FeedbackCycleRequest>>);

impl FeedbackCyclePort for Feedback {
    fn request_feedback_cycle(&self, request: FeedbackCycleRequest) -> FeedbackCycleResponse {
        self.0.borrow_mut().push(request);
        FeedbackCycleResponse::Accepted
    }
}

pub(super) struct Semantics;

impl SemanticProviderPort for Semantics {
    fn definition(
        &self,
        _root: &AdmittedRoot,
        uri: &str,
        _position: LspPosition,
    ) -> super::super::gateway::SemanticProviderOutcome<Vec<LspLocation>> {
        SemanticProviderOutcome::Complete(vec![LspLocation {
            uri: uri.into(),
            range: LspRange {
                start: LspPosition {
                    line: 0,
                    character: 0,
                },
                end: LspPosition {
                    line: 0,
                    character: 0,
                },
            },
        }])
    }
}

pub(super) struct Diagnostics;

impl DiagnosticSnapshotPort for Diagnostics {
    fn document_diagnostics(
        &self,
        _root: &AdmittedRoot,
        uri: &str,
        overlay: Option<&OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        assert!(overlay.is_none_or(|overlay| overlay.ephemeral));
        DiagnosticSnapshotOutcome::Ready {
            diagnostics: GenerationDiagnostics {
                generation: 9,
                upstream: vec![GatewayDiagnostic {
                    uri: uri.into(),
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
                    code: Some("warning".into()),
                    code_description_uri: None,
                    message: "bounded diagnostic".into(),
                    source: DiagnosticSource::Upstream,
                    related_information: Vec::new(),
                    data: None,
                }],
                tracedecay: Vec::new(),
            },
            completed_operation_id: None,
        }
    }
}

#[derive(Clone, Default)]
struct CapturingContext {
    document_content_digest: Arc<Mutex<Option<ContentDigest>>>,
}

impl ContextProjectionPort for CapturingContext {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        vec![ContextProjectionRegistration {
            kind: ContextProjectionKind::test_run_results(),
            revision: TRACEDECAY_CONTEXT_REVISION,
        }]
    }

    fn snapshot(
        &self,
        _root: &AdmittedRoot,
        _request_id: &LspRequestId,
        request: &ContextProjectionRequest,
    ) -> ContextProjectionOutcome {
        *self
            .document_content_digest
            .lock()
            .expect("capture context request") = request.document_content_digest.clone();
        ContextProjectionOutcome::Deferred {
            reason: "captured".to_owned(),
        }
    }
}

pub(super) fn session() -> DaemonLspProtocolSession<Feedback, Semantics, Diagnostics> {
    let capabilities = GatewayCapabilities::default();
    let upstream = UpstreamCapabilities {
        supports_diagnostics: true,
        semantic: SemanticCapability::ALL.into_iter().collect(),
    };
    let effective = negotiate_capabilities(
        &ClientCapabilities {
            supports_versioned_publish_diagnostics: true,
            publish_diagnostics_related_information: true,
            publish_diagnostics_code_description: true,
            publish_diagnostics_data: true,
            supports_document_diagnostics: true,
            workspace_diagnostic_refresh_support: true,
            semantic: SemanticCapability::ALL.into_iter().collect(),
            ..ClientCapabilities::default()
        },
        &capabilities,
        &upstream,
    );
    DaemonLspProtocolSession::new(
        DaemonLspGateway::with_semantic_provider(
            AdmittedRoot::new("file:///root"),
            effective,
            Feedback::default(),
            Semantics,
        ),
        capabilities,
        upstream,
        Diagnostics,
    )
}

pub(super) fn initialize(session: &mut DaemonLspProtocolSession<Feedback, Semantics, Diagnostics>) {
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "rootUri": "file:///root",
            "capabilities": {
                "general": { "positionEncodings": ["utf-16"] },
                "textDocument": {
                    "publishDiagnostics": {
                        "versionSupport": true,
                        "relatedInformation": true,
                        "codeDescriptionSupport": true,
                        "dataSupport": true
                    },
                    "diagnostic": {
                        "relatedInformation": true,
                        "codeDescriptionSupport": true,
                        "dataSupport": true
                    },
                    "definition": {},
                    "declaration": {},
                    "typeDefinition": {},
                    "implementation": {},
                    "references": {},
                    "hover": {},
                    "documentSymbol": {},
                    "signatureHelp": {},
                    "callHierarchy": {},
                    "typeHierarchy": {}
                },
                "workspace": {
                    "symbol": {},
                    "diagnostic": { "refreshSupport": true }
                }
            }
        }
    });
    session.handle_payload(&serde_json::to_vec(&request).unwrap(), 0);
    let initial = session.drain_outbound();
    assert_eq!(initial.len(), 1);
    let response: Value = serde_json::from_slice(&initial[0]).unwrap();
    assert!(
        response["result"]["capabilities"]
            .get("renameProvider")
            .is_none()
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    assert_eq!(session.lifecycle(), SessionLifecycle::Ready);
}

#[test]
fn initialization_is_single_root_and_deferred_methods_are_typed_unavailable() {
    let mut session = session();
    initialize(&mut session);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"textDocument/rename","params":{}}"#,
        2,
    );
    let output = session.drain_outbound();
    let response: Value = serde_json::from_slice(&output[0]).unwrap();
    assert_eq!(response["error"]["code"], -32601);
    assert_eq!(response["error"]["data"]["reason"], "explicitlyUnavailable");
}

#[derive(Clone, Default)]
struct RoutingSemantics {
    routed_scope_digests: Arc<Mutex<Vec<ManifestDigest>>>,
}

impl SemanticProviderPort for RoutingSemantics {
    fn definition(
        &self,
        root: &AdmittedRoot,
        uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        self.routed_scope_digests
            .lock()
            .expect("capture routed root")
            .push(root.scope_digest().expect("authorized root").clone());
        SemanticProviderOutcome::Complete(vec![LspLocation {
            uri: uri.to_owned(),
            range: LspRange {
                start: LspPosition {
                    line: 0,
                    character: 0,
                },
                end: LspPosition {
                    line: 0,
                    character: 0,
                },
            },
        }])
    }

    fn workspace_symbols(
        &self,
        root: &AdmittedRoot,
        _query: &str,
    ) -> SemanticProviderOutcome<Vec<WorkspaceSymbol>> {
        self.routed_scope_digests
            .lock()
            .expect("capture routed root")
            .push(root.scope_digest().expect("authorized root").clone());
        SemanticProviderOutcome::Complete(vec![WorkspaceSymbol {
            name: root.uri().to_owned(),
            kind: 1,
            location: LspLocation {
                uri: root.uri().to_owned(),
                range: LspRange {
                    start: LspPosition {
                        line: 0,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 0,
                        character: 0,
                    },
                },
            },
        }])
    }
}

#[test]
fn two_root_session_routes_documents_and_workspace_requests_to_exact_roots() {
    let left_digest = ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap();
    let right_digest = ManifestDigest::new(format!("sha256:{}", "b".repeat(64))).unwrap();
    let workspace = AuthorizedLspWorkspace::new(
        Some(ManifestDigest::new(format!("sha256:{}", "c".repeat(64))).unwrap()),
        vec![
            AdmittedRoot::authorized("file:///left", left_digest.clone()),
            AdmittedRoot::authorized("file:///right", right_digest.clone()),
        ],
    )
    .unwrap();
    let semantics = RoutingSemantics::default();
    let routed = semantics.routed_scope_digests.clone();
    let gateway_capabilities = GatewayCapabilities {
        supports_workspace_folders: true,
        ..GatewayCapabilities::default()
    };
    let upstream = UpstreamCapabilities {
        supports_diagnostics: true,
        semantic: SemanticCapability::ALL.into_iter().collect(),
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
        semantics,
        Diagnostics,
    );
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "workspaceFolders": [
                { "uri": "file:///left", "name": "left" },
                { "uri": "file:///right", "name": "right" }
            ],
            "capabilities": {
                "general": { "positionEncodings": ["utf-16"] },
                "textDocument": {
                    "definition": {},
                    "diagnostic": {}
                },
                "workspace": {
                    "workspaceFolders": true,
                    "symbol": {}
                }
            }
        }
    });
    session.handle_payload(&serde_json::to_vec(&initialize).unwrap(), 0);
    session.drain_outbound();
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );

    for (id, uri) in [
        (2, "file:///left/src/lib.rs"),
        (3, "file:///right/src/lib.rs"),
    ] {
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "textDocument/definition",
            "params": {
                "textDocument": { "uri": uri },
                "position": { "line": 0, "character": 0 }
            }
        });
        session.handle_payload(&serde_json::to_vec(&request).unwrap(), id);
    }
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":4,"method":"workspace/symbol","params":{"query":"needle"}}"#,
        4,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":5,"method":"textDocument/diagnostic","params":{"textDocument":{"uri":"file:///left/src/lib.rs"}}}"#,
        5,
    );
    let responses = session.drain_outbound();
    assert_eq!(responses.len(), 4);
    let workspace_response: Value = serde_json::from_slice(&responses[2]).unwrap();
    assert_eq!(
        workspace_response["result"]
            .as_array()
            .unwrap_or_else(|| panic!("workspace response was {workspace_response}"))
            .len(),
        2
    );
    let diagnostic_response: Value = serde_json::from_slice(&responses[3]).unwrap();
    let result_id = diagnostic_response["result"]["resultId"].as_str().unwrap();
    assert!(result_id.contains(&"c".repeat(64)));
    assert!(result_id.contains(&"a".repeat(64)));
    assert!(result_id.contains("root=0"));
    assert_eq!(
        *routed.lock().expect("read routed roots"),
        vec![
            left_digest.clone(),
            right_digest.clone(),
            left_digest,
            right_digest,
        ]
    );

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":6,"method":"textDocument/definition","params":{"textDocument":{"uri":"file:///escape.rs"},"position":{"line":0,"character":0}}}"#,
        6,
    );
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(response["error"]["data"]["reason"], "outsideAdmittedRoot");
}

#[test]
fn failed_initialize_does_not_transition_or_admit_document_content() {
    let mut session = session();
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "rootUri": "file:///not-admitted",
            "capabilities": { "general": { "positionEncodings": ["utf-16"] } }
        }
    });
    session.handle_payload(&serde_json::to_vec(&request).unwrap(), 0);
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(session.lifecycle(), SessionLifecycle::AwaitingInitialize);

    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"secret"}}}"#,
            2,
        );
    assert_eq!(session.lifecycle(), SessionLifecycle::AwaitingInitialize);
    assert!(session.overlays().snapshot("file:///root/a.rs").is_none());

    initialize(&mut session);
}

#[test]
fn context_request_binds_exact_session_overlay_digest() {
    let document_uri = "file:///root/a.rs";
    let mut overlays = OverlayStore::default();
    overlays
        .open(document_uri, "rust", 1, "fn dirty() {}")
        .expect("open overlay");
    let mut request = ContextProjectionRequest {
        kind: ContextProjectionKind::test_run_results(),
        document_uri: Some(document_uri.to_owned()),
        document_content_digest: None,
    };

    bind_context_document_digest(&mut request, &overlays);

    assert_eq!(
        request.document_content_digest,
        Some(ContentDigest::of_bytes(b"fn dirty() {}"))
    );
    assert!(
        serde_json::from_value::<ContextProjectionRequest>(json!({
            "kind": "testRunResults",
            "documentUri": document_uri,
            "documentContentDigest": format!("sha256:{}", "a".repeat(64)),
        }))
        .is_err(),
        "clients cannot supply the trusted overlay digest"
    );
}

#[test]
fn document_change_invalidates_context_currentness_before_expansion() {
    let document_uri = "file:///root/a.rs";
    let mut session = session();
    initialize(&mut session);
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"fn a() {}"}}}"#,
            1,
        );
    session.context.currentness.insert(
        (
            ContextProjectionKind::test_run_results(),
            Some(document_uri.to_owned()),
        ),
        ContextProjectionCurrentness {
            generation: 7,
            identity: ContextProjectionIdentity {
                head_commit_id: "0123456789abcdef".to_owned(),
                code_generation_id: "generation:7".to_owned(),
                snapshot_digest: format!("sha256:{}", "a".repeat(64)),
                invalidation_digest: format!("sha256:{}", "b".repeat(64)),
                snapshot_content_digest: format!("sha256:{}", "c".repeat(64)),
                document_content_digest: Some(format!("sha256:{}", "d".repeat(64))),
            },
        },
    );

    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///root/a.rs","version":2},"contentChanges":[{"text":"fn b() {}"}]}}"#,
            2,
        );

    assert!(
        !session
            .context
            .currentness
            .keys()
            .any(|(_, uri)| uri.as_deref() == Some(document_uri))
    );
}

#[test]
fn context_dispatch_supplies_session_overlay_digest_to_canonical_reader() {
    let captured = CapturingContext::default();
    let observed = Arc::clone(&captured.document_content_digest);
    let mut capabilities = GatewayCapabilities::default();
    capabilities.context_projections.insert(
        ContextProjectionKind::test_run_results(),
        TRACEDECAY_CONTEXT_REVISION,
    );
    let upstream = UpstreamCapabilities::default();
    let effective =
        negotiate_capabilities(&ClientCapabilities::default(), &capabilities, &upstream);
    let mut session = DaemonLspProtocolSession::new(
        DaemonLspGateway::with_semantic_provider(
            AdmittedRoot::new("file:///root"),
            effective,
            Feedback::default(),
            Semantics,
        ),
        capabilities,
        upstream,
        Diagnostics,
    )
    .with_context_projection_port(captured);
    session.handle_payload(
        &serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "rootUri": "file:///root",
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "experimental": {
                        "tracedecay": {
                            "revision": TRACEDECAY_CONTEXT_REVISION,
                            "projections": [{
                                "kind": "testRunResults",
                                "revision": TRACEDECAY_CONTEXT_REVISION
                            }],
                            "opaqueExpansion": false
                        }
                    }
                }
            }
        }))
        .expect("initialize request"),
        0,
    );
    session.drain_outbound();
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"fn dirty() {}"}}}"#,
            2,
        );
    session.handle_payload(
            br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"testRunResults","documentUri":"file:///root/a.rs"}}"#,
            3,
        );

    assert_eq!(
        *observed.lock().expect("read captured context request"),
        Some(ContentDigest::of_bytes(b"fn dirty() {}"))
    );
}

#[test]
fn malformed_position_encoding_initialize_is_retryable() {
    let mut session = session();
    session.handle_payload(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":"utf-16"}}}}"#,
            0,
        );
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(session.lifecycle(), SessionLifecycle::AwaitingInitialize);
    initialize(&mut session);
}

#[test]
fn outbound_backpressure_cannot_half_commit_initialize() {
    let mut session = session();
    session.outbound.queue.push_back(QueuedFrame {
        payload: vec![0; MAX_QUEUED_OUTBOUND_BYTES],
        publication: None,
        server_request: None,
    });
    session.outbound.queued_bytes = MAX_QUEUED_OUTBOUND_BYTES;
    session.handle_payload(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#,
            0,
        );
    assert_eq!(session.lifecycle(), SessionLifecycle::AwaitingInitialize);
    session.drain_outbound();
    initialize(&mut session);
}
