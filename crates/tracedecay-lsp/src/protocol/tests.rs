use super::context_controller::ContextProjectionCurrentness;
use super::*;
use crate::TRACEDECAY_CONTEXT_REVISION;
use crate::bridge::{DaemonLspSessionTransport, FramePoll, FrameSend};
use crate::capabilities::SemanticCapability;
use crate::diagnostics::{DiagnosticSeverity, DiagnosticSource, LspPosition, LspRange};
use crate::gateway::{FeedbackCycleRequest, LspLocation, SemanticProviderOutcome, WorkspaceSymbol};
use crate::overlay::{MAX_OVERLAY_BYTES, OVERLAY_DIAGNOSTIC_DEBOUNCE_MS, OverlaySnapshot};
use crate::provider::GenerationDiagnostics;
use std::cell::RefCell;
use std::sync::Mutex;
use tracedecay_domain::ManifestDigest;

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

#[test]
fn save_flushes_pending_overlay_diagnostics() {
    let mut session = session();
    initialize(&mut session);
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"fn a() {}"}}}"#,
            10,
        );
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didSave","params":{"textDocument":{"uri":"file:///root/a.rs"}}}"#,
            11,
        );

    let messages = session.drain_outbound();
    let publication: Value = messages
        .iter()
        .map(|message| serde_json::from_slice(message).unwrap())
        .find(|message: &Value| message["method"] == "textDocument/publishDiagnostics")
        .unwrap();
    assert_eq!(publication["params"]["version"], 1);
}

#[test]
fn publish_diagnostics_omits_unnegotiated_optional_fields() {
    let mut session = session();
    session.handle_payload(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":["utf-16"]},"textDocument":{"publishDiagnostics":{"versionSupport":true}}}}}"#,
            0,
        );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":4,"text":"fn a() {}"}}}"#,
            2,
        );
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didSave","params":{"textDocument":{"uri":"file:///root/a.rs"}}}"#,
            3,
        );

    let publication: Value = session
        .drain_outbound()
        .iter()
        .map(|message| serde_json::from_slice(message).unwrap())
        .find(|message: &Value| message["method"] == "textDocument/publishDiagnostics")
        .expect("baseline publish diagnostics remains available");
    assert_eq!(publication["params"]["version"], 4);
    let diagnostic = &publication["params"]["diagnostics"][0];
    assert!(diagnostic.get("relatedInformation").is_none());
    assert!(diagnostic.get("codeDescription").is_none());
    assert!(diagnostic.get("data").is_none());
}

#[test]
fn related_locations_are_limited_to_the_admitted_root() {
    let session = session();
    let diagnostic = GatewayDiagnostic {
        uri: "file:///root/a.rs".to_owned(),
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
        severity: Some(DiagnosticSeverity::Information),
        code: Some("github-review".to_owned()),
        code_description_uri: None,
        message: "review".to_owned(),
        source: DiagnosticSource::TraceDecayGitHub,
        related_information: vec![
            super::super::diagnostics::GatewayDiagnosticRelatedInformation {
                uri: "file:///root/caller.rs".to_owned(),
                range: LspRange {
                    start: LspPosition {
                        line: 1,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 1,
                        character: 1,
                    },
                },
                message: "authorized".to_owned(),
            },
            super::super::diagnostics::GatewayDiagnosticRelatedInformation {
                uri: "file:///other/secret.rs".to_owned(),
                range: LspRange {
                    start: LspPosition {
                        line: 1,
                        character: 0,
                    },
                    end: LspPosition {
                        line: 1,
                        character: 1,
                    },
                },
                message: "outside root".to_owned(),
            },
        ],
        data: None,
    };

    let visible = session.visible_diagnostics(vec![diagnostic], true);
    assert_eq!(visible[0].related_information.len(), 1);
    assert_eq!(
        visible[0].related_information[0].uri,
        "file:///root/caller.rs"
    );
}

#[test]
fn managed_diagnostics_require_negotiated_data_identity() {
    let session = session();
    let diagnostic = |source| GatewayDiagnostic {
        uri: "file:///root/a.rs".to_owned(),
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
        severity: Some(DiagnosticSeverity::Information),
        code: Some("finding".to_owned()),
        code_description_uri: None,
        message: "finding".to_owned(),
        source,
        related_information: Vec::new(),
        data: None,
    };

    let visible = session.visible_diagnostics(
        vec![
            diagnostic(DiagnosticSource::Upstream),
            diagnostic(DiagnosticSource::TraceDecayGitHub),
        ],
        false,
    );

    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].source, DiagnosticSource::Upstream);
}

#[test]
fn cursor_native_diagnostics_are_merged_but_not_republished() {
    let mut session = session();
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "rootUri": "file:///root",
            "initializationOptions": {
                "tracedecay": {
                    "mode": "cursor-native",
                    "context": true
                }
            },
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
                    }
                },
                "workspace": {
                    "diagnostic": { "refreshSupport": true }
                }
            }
        }
    });
    session.handle_payload(&serde_json::to_vec(&request).unwrap(), 0);
    session.drain_outbound();
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":7,"text":"fn a() {}"}}}"#,
            10,
        );
    session.drain_outbound();
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"tracedecay/nativeDiagnostics","params":{"uri":"file:///root/a.rs","version":7,"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"tracedecay","message":"projected diagnostic"},{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"TraceDecay-CI","message":"projected CI diagnostic"},{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"tracedecay-proximity","message":"projected proximity diagnostic"},{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"typescript","message":"native diagnostic","data":{"ruleId":"typescript"}}]}}"#,
            11,
        );
    assert_eq!(
        session
            .diagnostics
            .native_upstream
            .get("file:///root/a.rs")
            .unwrap()
            .diagnostics
            .len(),
        1
    );
    assert_eq!(
        session.diagnostics.native_upstream["file:///root/a.rs"].diagnostics[0].message,
        "native diagnostic",
        "TraceDecay-projected sources must never be inverted into native upstream evidence"
    );
    session.detach().unwrap();
    session.reconnect().unwrap();
    assert!(
        session
            .diagnostics
            .native_upstream
            .contains_key("file:///root/a.rs"),
        "reconnect preserves the current in-memory native diagnostic lane"
    );
    session.flush_due(61);

    let messages = session.drain_outbound();
    let publication: Value = messages
        .iter()
        .map(|message| serde_json::from_slice(message).unwrap())
        .find(|message: &Value| message["method"] == "textDocument/publishDiagnostics")
        .unwrap();
    assert!(
        publication["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty(),
        "Cursor-native clients must not receive their own diagnostics back"
    );
    let projected_only = br#"{"jsonrpc":"2.0","method":"tracedecay/nativeDiagnostics","params":{"uri":"file:///root/a.rs","version":7,"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"tracedecay-github","message":"projected diagnostic"}]}}"#;
    session.handle_payload(projected_only, 62);
    session.flush_due(62);
    session.drain_outbound();
    assert!(
        session.diagnostics.native_upstream["file:///root/a.rs"]
            .diagnostics
            .is_empty(),
        "a projected-only native publication clears prior upstream evidence once"
    );
    session.handle_payload(projected_only, 63);
    assert_eq!(
        session.flush_due(10_000).queued_messages,
        0,
        "an unchanged projected-only publication must not start a refresh loop"
    );
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"tracedecay/nativeDiagnostics","params":{"uri":"file:///root/a.rs","version":7,"diagnostics":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"source":"typescript","message":"changed native diagnostic"}]}}"#,
            10_001,
        );
    assert_eq!(
        session.diagnostics.native_upstream["file:///root/a.rs"].diagnostics[0].message,
        "changed native diagnostic",
        "duplicate suppression must not suppress a real native evidence change"
    );
    session.drain_outbound();
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":"file:///root/a.rs"}}}"#,
            10_002,
        );
    assert!(
        !session
            .diagnostics
            .native_upstream
            .contains_key("file:///root/a.rs"),
        "closing a document clears its native diagnostic lane"
    );
}

#[test]
fn exit_releases_session_local_overlays_and_queued_frames() {
    let mut session = session();
    initialize(&mut session);
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"secret"}}}"#,
            2,
        );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}"#,
        3,
    );
    session.handle_payload(br#"{"jsonrpc":"2.0","method":"exit","params":{}}"#, 4);

    assert_eq!(session.lifecycle(), SessionLifecycle::Exited);
    assert!(session.overlays().snapshot("file:///root/a.rs").is_none());
    assert!(session.drain_outbound().is_empty());
}

#[test]
fn overlays_debounce_publish_and_do_not_become_clean_generation_state() {
    let mut session = session();
    initialize(&mut session);
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"fn a() {}"}}}"#,
            10,
        );
    session.drain_outbound();
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///root/a.rs","version":2},"contentChanges":[{"text":"fn b() {}"}]}}"#,
            40,
        );
    assert_eq!(
        session
            .flush_due(40 + OVERLAY_DIAGNOSTIC_DEBOUNCE_MS - 1)
            .queued_messages,
        0
    );
    let output = session.flush_due(40 + OVERLAY_DIAGNOSTIC_DEBOUNCE_MS);
    assert!(output.queued_messages >= 1);
    let messages = session.drain_outbound();
    let publication: Value = messages
        .iter()
        .map(|message| serde_json::from_slice(message).unwrap())
        .find(|message: &Value| message["method"] == "textDocument/publishDiagnostics")
        .unwrap();
    assert_eq!(publication["params"]["version"], 2);
    assert!(
        session
            .overlays()
            .snapshot("file:///root/a.rs")
            .unwrap()
            .ephemeral
    );
}

#[test]
fn close_then_reopen_resets_debounce_and_publication_version_ordering() {
    let mut session = session();
    initialize(&mut session);
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":10,"text":"old"}}}"#,
            10,
        );
    session.flush_due(10 + OVERLAY_DIAGNOSTIC_DEBOUNCE_MS);
    session.drain_outbound();
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":"file:///root/a.rs"}}}"#,
            61,
        );
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"new"}}}"#,
            62,
        );
    session.flush_due(62 + OVERLAY_DIAGNOSTIC_DEBOUNCE_MS);

    let messages = session.drain_outbound();
    let publication: Value = messages
        .iter()
        .map(|message| serde_json::from_slice(message).unwrap())
        .find(|message: &Value| message["method"] == "textDocument/publishDiagnostics")
        .unwrap();
    assert_eq!(publication["params"]["version"], 1);
    assert_eq!(
        publication["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn server_refresh_responses_do_not_create_json_rpc_response_loops() {
    let mut session = session();
    initialize(&mut session);
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"x"}}}"#,
            10,
        );
    session.flush_due(10 + OVERLAY_DIAGNOSTIC_DEBOUNCE_MS);
    let messages = session.drain_outbound();
    let refresh: Value = messages
        .iter()
        .map(|message| serde_json::from_slice(message).unwrap())
        .find(|message: &Value| message["method"] == "workspace/diagnostic/refresh")
        .unwrap();
    let response = json!({
        "jsonrpc": "2.0",
        "id": refresh["id"].clone(),
        "result": null,
    });
    session.handle_payload(&serde_json::to_vec(&response).unwrap(), 61);
    assert!(session.drain_outbound().is_empty());
}

#[test]
fn exit_request_is_rejected_without_closing_the_session() {
    let mut session = session();
    initialize(&mut session);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":9,"method":"exit","params":{}}"#,
        2,
    );
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(response["error"]["code"], -32600);
    assert_eq!(session.lifecycle(), SessionLifecycle::Ready);
}

#[test]
fn pull_diagnostics_are_generation_bound_and_return_unchanged_for_same_result_id() {
    let mut session = session();
    initialize(&mut session);
    session.handle_payload(
            br#"{"jsonrpc":"2.0","id":3,"method":"textDocument/diagnostic","params":{"textDocument":{"uri":"file:///root/a.rs"}}}"#,
            10,
        );
    let first = session.drain_outbound();
    let first: Value = serde_json::from_slice(&first[0]).unwrap();
    let result_id = first["result"]["resultId"].as_str().unwrap().to_owned();
    assert_eq!(result_id, "generation:9:version:0");
    let request = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "textDocument/diagnostic",
        "params": {
            "textDocument": { "uri": "file:///root/a.rs" },
            "previousResultId": result_id,
        },
    });
    session.handle_payload(&serde_json::to_vec(&request).unwrap(), 11);
    let second = session.drain_outbound();
    let second: Value = serde_json::from_slice(&second[0]).unwrap();
    assert_eq!(second["result"]["kind"], "unchanged");
}

#[test]
fn bridge_transport_parses_typed_session_frames_and_acks_delivery() {
    let mut transport = DaemonLspProtocolTransport::new(session());
    transport.set_now_ms(0);
    assert_eq!(
            transport
                .try_send_client_frame(
                    br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#,
                )
                .unwrap(),
            FrameSend::Sent
        );
    assert!(matches!(
        transport.poll_daemon_frame().unwrap(),
        FramePoll::Frame(frame) if serde_json::from_slice::<Value>(&frame).unwrap()["id"] == 1
    ));
    transport.acknowledge_daemon_frame().unwrap();
    assert_eq!(transport.poll_daemon_frame().unwrap(), FramePoll::Pending);
}

#[test]
fn in_flight_publication_is_not_replaced_or_used_to_ack_a_newer_version() {
    let mut session = session();
    initialize(&mut session);
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"a"}}}"#,
            10,
        );
    assert!(session.poll_outbound().is_some());
    session.handle_payload(
            br#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///root/a.rs","version":2},"contentChanges":[{"text":"b"}]}}"#,
            11,
        );
    session.flush_due(11 + OVERLAY_DIAGNOSTIC_DEBOUNCE_MS);

    assert_eq!(
        session
            .outbound
            .queue
            .iter()
            .filter(|frame| frame.publication.is_some())
            .count(),
        2
    );
    assert!(session.acknowledge_outbound());
    assert_eq!(
        session
            .lifecycle
            .control
            .publication("file:///root/a.rs")
            .unwrap()
            .delivery,
        super::super::session::PublicationDelivery::Queued
    );
    assert!(session.poll_outbound().is_some());
    assert!(session.acknowledge_outbound());
    assert_eq!(
        session
            .lifecycle
            .control
            .publication("file:///root/a.rs")
            .unwrap()
            .delivery,
        super::super::session::PublicationDelivery::BridgeAcknowledged
    );
}

#[test]
fn oversized_overlay_closes_before_the_bridge_acknowledges_the_notification() {
    let mut transport = DaemonLspProtocolTransport::new(session());
    transport.session_mut().handle_payload(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///root","capabilities":{"general":{"positionEncodings":["utf-16"]}}}}"#,
            0,
        );
    transport.session_mut().drain_outbound();
    transport.session_mut().handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );

    let notification = json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": "file:///root/oversized.rs",
                "languageId": "rust",
                "version": 1,
                "text": "x".repeat(MAX_OVERLAY_BYTES + 1),
            }
        }
    });
    let notification = serde_json::to_vec(&notification).unwrap();

    assert_eq!(
        transport.try_send_client_frame(&notification).unwrap(),
        FrameSend::Closed
    );
    assert_eq!(transport.session().lifecycle(), SessionLifecycle::Expired);
    assert!(
        transport
            .session()
            .overlays()
            .snapshot("file:///root/oversized.rs")
            .is_none()
    );
}
