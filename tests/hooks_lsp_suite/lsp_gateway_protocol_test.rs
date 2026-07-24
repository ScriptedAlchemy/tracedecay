use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use tracedecay::daemon::lsp_gateway::{
    AdmittedRoot, ClientCapabilities, ContextCoverage, ContextExpansionEnvelope,
    ContextExpansionOutcome, ContextExpansionRequest, ContextExpansionScope, ContextFreshness,
    ContextProducerState, ContextProjectionEnvelope, ContextProjectionIdentity,
    ContextProjectionKind, ContextProjectionOutcome, ContextProjectionPort,
    ContextProjectionRegistration, ContextProjectionRequest, DaemonLspGateway,
    DaemonLspProtocolSession, FeedbackCyclePort, FeedbackCycleRequest, FeedbackCycleResponse,
    GatewayCapabilities, LspRequestId, SemanticProviderPort, TRACEDECAY_CONTEXT_REVISION,
    UnavailableDiagnosticSnapshotProvider, UpstreamCapabilities, negotiate_capabilities,
};

struct Feedback;

impl FeedbackCyclePort for Feedback {
    fn request_feedback_cycle(&self, _request: FeedbackCycleRequest) -> FeedbackCycleResponse {
        FeedbackCycleResponse::Accepted
    }
}

struct Semantics;

impl SemanticProviderPort for Semantics {}

fn session() -> DaemonLspProtocolSession<Feedback, Semantics, UnavailableDiagnosticSnapshotProvider>
{
    let gateway_capabilities = GatewayCapabilities::default();
    let upstream_capabilities = UpstreamCapabilities::default();
    let effective = negotiate_capabilities(
        &ClientCapabilities::default(),
        &gateway_capabilities,
        &upstream_capabilities,
    );
    DaemonLspProtocolSession::without_diagnostic_provider(
        DaemonLspGateway::with_semantic_provider(
            AdmittedRoot::new("file:///root"),
            effective,
            Feedback,
            Semantics,
        ),
        gateway_capabilities,
        upstream_capabilities,
    )
}

fn initialize(
    session: &mut DaemonLspProtocolSession<
        Feedback,
        Semantics,
        UnavailableDiagnosticSnapshotProvider,
    >,
) {
    let initialize = json!({
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
                    }
                }
            }
        }
    });
    session.handle_payload(&serde_json::to_vec(&initialize).unwrap(), 0);
    let response: Value = session
        .drain_outbound()
        .into_iter()
        .map(|message| serde_json::from_slice(&message).unwrap())
        .find(|message: &Value| message["id"] == 1)
        .expect("initialize response should be present");
    assert_eq!(
        response["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
}

#[test]
fn lsp_protocol_keeps_unsaved_edits_session_local_and_rejects_deferred_methods() {
    let mut session = session();
    initialize(&mut session);

    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"x"}}}"#,
        2,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///root/a.rs","version":2},"contentChanges":[{"text":""}]}}"#,
        3,
    );
    let overlay = session.overlays().snapshot("file:///root/a.rs").unwrap();
    assert!(overlay.ephemeral);
    assert_eq!(overlay.text, "");

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"textDocument/rename","params":{}}"#,
        4,
    );
    let response: Value = session
        .drain_outbound()
        .into_iter()
        .map(|message| serde_json::from_slice(&message).unwrap())
        .find(|message: &Value| message["id"] == 2)
        .expect("rename response should not depend on notification queue order");
    assert_eq!(response["error"]["code"], -32601);
    assert_eq!(response["error"]["data"]["reason"], "explicitlyUnavailable");
}

#[derive(Default)]
struct PendingContext {
    polls: AtomicUsize,
}

fn fixture_projection_identity() -> ContextProjectionIdentity {
    ContextProjectionIdentity {
        head_commit_id: "0123456789abcdef".to_owned(),
        code_generation_id: "generation:1".to_owned(),
        snapshot_digest: format!("sha256:{}", "a".repeat(64)),
        invalidation_digest: format!("sha256:{}", "b".repeat(64)),
        snapshot_content_digest: format!("sha256:{}", "c".repeat(64)),
        document_content_digest: None,
    }
}

impl ContextProjectionPort for PendingContext {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        vec![ContextProjectionRegistration {
            kind: ContextProjectionKind::diagnostics(),
            revision: TRACEDECAY_CONTEXT_REVISION,
        }]
    }

    fn snapshot(
        &self,
        _root: &AdmittedRoot,
        _request_id: &LspRequestId,
        _request: &ContextProjectionRequest,
    ) -> ContextProjectionOutcome {
        ContextProjectionOutcome::Pending
    }

    fn poll_snapshot(
        &self,
        root: &AdmittedRoot,
        _request_id: &LspRequestId,
    ) -> Option<ContextProjectionOutcome> {
        if self.polls.fetch_add(1, Ordering::SeqCst) == 0 {
            return None;
        }
        Some(ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
            root_uri: root.uri().to_owned(),
            document_uri: None,
            kind: ContextProjectionKind::diagnostics(),
            generation: 1,
            identity: fixture_projection_identity(),
            freshness: ContextFreshness::Current,
            producer_state: ContextProducerState::Complete,
            coverage: ContextCoverage::Complete,
            revision: TRACEDECAY_CONTEXT_REVISION,
            items: Vec::new(),
            omitted_count: 0,
            omission_reasons: Vec::new(),
            retrieval_handle: None,
        }))
    }

    fn expand(
        &self,
        root: &AdmittedRoot,
        _request_id: &LspRequestId,
        request: &ContextExpansionRequest,
    ) -> ContextExpansionOutcome {
        if request.retrieval_handle == "rh_stale" {
            return ContextExpansionOutcome::Ready(ContextExpansionEnvelope {
                root_uri: root.uri().to_owned(),
                document_uri: None,
                kind: ContextProjectionKind::diagnostics(),
                stable_id: "finding.1".to_owned(),
                generation: 1,
                scope: ContextExpansionScope {
                    scope_digest: "sha256:scope".to_owned(),
                    identity: fixture_projection_identity(),
                },
                expires_at: 10_000,
                coverage: ContextCoverage::Partial,
                revision: TRACEDECAY_CONTEXT_REVISION,
                evidence: None,
                omission_reason: Some("stale-generation".to_owned()),
            });
        }
        if request.retrieval_handle != "rh_current" {
            return ContextExpansionOutcome::Denied;
        }
        ContextExpansionOutcome::Ready(ContextExpansionEnvelope {
            root_uri: root.uri().to_owned(),
            document_uri: None,
            kind: ContextProjectionKind::diagnostics(),
            stable_id: "finding.1".to_owned(),
            generation: 1,
            scope: ContextExpansionScope {
                scope_digest: "sha256:scope".to_owned(),
                identity: fixture_projection_identity(),
            },
            expires_at: 10_000,
            coverage: ContextCoverage::Complete,
            revision: TRACEDECAY_CONTEXT_REVISION,
            evidence: Some(json!({ "canonical": "feedback-expand" })),
            omission_reason: None,
        })
    }
}

#[test]
fn lsp_context_request_stays_correlated_until_async_projection_completes() {
    let mut gateway_capabilities = GatewayCapabilities::default();
    gateway_capabilities.context_projections.insert(
        ContextProjectionKind::diagnostics(),
        TRACEDECAY_CONTEXT_REVISION,
    );
    let upstream_capabilities = UpstreamCapabilities::default();
    let effective = negotiate_capabilities(
        &ClientCapabilities::default(),
        &gateway_capabilities,
        &upstream_capabilities,
    );
    let mut session = DaemonLspProtocolSession::without_diagnostic_provider(
        DaemonLspGateway::with_semantic_provider(
            AdmittedRoot::new("file:///root"),
            effective,
            Feedback,
            Semantics,
        ),
        gateway_capabilities,
        upstream_capabilities,
    )
    .with_context_projection_port(PendingContext::default());

    let initialize = json!({
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
                        "opaqueExpansion": true,
                        "projections": [{
                            "kind": "diagnostics",
                            "revision": TRACEDECAY_CONTEXT_REVISION
                        }]
                    }
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

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"diagnostics"}}"#,
        2,
    );
    assert!(session.drain_outbound().is_empty());

    session.flush_due(3);
    let response: Value = session
        .drain_outbound()
        .into_iter()
        .map(|message| serde_json::from_slice(&message).unwrap())
        .find(|message: &Value| message["id"] == 2)
        .expect("pending context request should complete on a later actor poll");
    assert_eq!(response["result"]["kind"], "diagnostics");
    assert_eq!(response["result"]["generation"], 1);
}

#[test]
fn lsp_context_expansion_is_namespaced_and_returns_canonical_evidence() {
    let mut gateway_capabilities = GatewayCapabilities::default();
    gateway_capabilities.context_projections.insert(
        ContextProjectionKind::diagnostics(),
        TRACEDECAY_CONTEXT_REVISION,
    );
    let upstream_capabilities = UpstreamCapabilities::default();
    let effective = negotiate_capabilities(
        &ClientCapabilities::default(),
        &gateway_capabilities,
        &upstream_capabilities,
    );
    let mut session = DaemonLspProtocolSession::without_diagnostic_provider(
        DaemonLspGateway::with_semantic_provider(
            AdmittedRoot::new("file:///root"),
            effective,
            Feedback,
            Semantics,
        ),
        gateway_capabilities,
        upstream_capabilities,
    )
    .with_context_projection_port(PendingContext::default());

    let initialize = json!({
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
                        "opaqueExpansion": true,
                        "projections": [{
                            "kind": "diagnostics",
                            "revision": TRACEDECAY_CONTEXT_REVISION
                        }]
                    }
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

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context/expand","params":{"retrievalHandle":"rh_current"}}"#,
        2,
    );
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(response["result"]["rootUri"], "file:///root");
    assert_eq!(response["result"]["generation"], 1);
    assert_eq!(
        response["result"]["evidence"]["canonical"],
        "feedback-expand"
    );

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":3,"method":"tracedecay/context/expand","params":{"retrievalHandle":"rh_stale"}}"#,
        3,
    );
    let stale: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(stale["result"]["coverage"], "partial");
    assert_eq!(stale["result"]["omissionReason"], "stale-generation");
    assert!(stale["result"].get("evidence").is_none());

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":4,"method":"tracedecay/context/expand","params":{"retrievalHandle":"rh_wrong_root"}}"#,
        4,
    );
    let denied: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(denied["error"]["code"], -32601);
}
