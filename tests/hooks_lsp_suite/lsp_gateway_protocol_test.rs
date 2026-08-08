use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use tracedecay_lsp::{
    AdmittedRoot, ClientCapabilities, ContextCoverage, ContextExpansionEnvelope,
    ContextExpansionOutcome, ContextExpansionRequest, ContextExpansionScope, ContextFreshness,
    ContextProducerState, ContextProjectionChange, ContextProjectionEnvelope,
    ContextProjectionIdentity, ContextProjectionKind, ContextProjectionOutcome,
    ContextProjectionPort, ContextProjectionRegistration, ContextProjectionRequest,
    DaemonLspGateway, DaemonLspProtocolSession, FeedbackCyclePort, FeedbackCycleRequest,
    FeedbackCycleResponse, GatewayCapabilities, LspRequestId, SemanticProviderPort,
    SessionLifecycle, TRACEDECAY_CONTEXT_REVISION, UnavailableDiagnosticSnapshotProvider,
    UpstreamCapabilities, negotiate_capabilities,
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
        br#"{"jsonrpc":"2.0","id":99,"method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"request-form"}}}"#,
        2,
    );
    let invalid_notification: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(invalid_notification["id"], 99);
    assert_eq!(invalid_notification["error"]["code"], -32600);
    assert!(session.overlays().snapshot("file:///root/a.rs").is_none());

    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"x"}}}"#,
        3,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"textDocument/didChange","params":{"textDocument":{"uri":"file:///root/a.rs","version":2},"contentChanges":[{"text":""}]}}"#,
        4,
    );
    let overlay = session.overlays().snapshot("file:///root/a.rs").unwrap();
    assert!(overlay.ephemeral);
    assert_eq!(&*overlay.text, "");

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"textDocument/rename","params":{}}"#,
        5,
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
    cancellations: AtomicUsize,
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

#[test]
fn lsp_context_identity_rejects_unknown_fields() {
    let mut identity =
        serde_json::to_value(fixture_projection_identity()).expect("serialize projection identity");
    identity
        .as_object_mut()
        .expect("projection identity object")
        .insert("unexpected".to_owned(), Value::Bool(true));
    assert!(serde_json::from_value::<ContextProjectionIdentity>(identity).is_err());
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
                next_retrieval_handle: None,
            });
        }
        if !matches!(request.retrieval_handle.as_str(), "rh_current" | "rh_paged") {
            return ContextExpansionOutcome::Denied;
        }
        let paged = request.retrieval_handle == "rh_paged";
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
            coverage: if paged {
                ContextCoverage::Partial
            } else {
                ContextCoverage::Complete
            },
            revision: TRACEDECAY_CONTEXT_REVISION,
            evidence: Some(json!({ "canonical": "feedback-expand" })),
            omission_reason: paged.then(|| "bounded-projection-items".to_owned()),
            next_retrieval_handle: paged.then(|| "rh_next_page".to_owned()),
        })
    }

    fn cancel_request(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
        self.cancellations.fetch_add(1, Ordering::SeqCst);
        true
    }
}

fn session_with_context<C>(
    context: C,
) -> DaemonLspProtocolSession<Feedback, Semantics, UnavailableDiagnosticSnapshotProvider>
where
    C: ContextProjectionPort + Send + Sync + 'static,
{
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
    .with_context_projection_port(context)
}

fn initialize_context(
    session: &mut DaemonLspProtocolSession<
        Feedback,
        Semantics,
        UnavailableDiagnosticSnapshotProvider,
    >,
    revision: u32,
) -> Value {
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
                        "revision": revision,
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
    let response = session
        .drain_outbound()
        .into_iter()
        .map(|message| serde_json::from_slice(&message).unwrap())
        .find(|message: &Value| message["id"] == 1)
        .expect("initialize response");
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    response
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

    for (id, request) in [
        (
            2,
            br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"diagnostics","unexpected":true}}"#
                .as_slice(),
        ),
        (
            3,
            br#"{"jsonrpc":"2.0","id":3,"method":"tracedecay/subscribe","params":{"projections":[],"unexpected":true}}"#
                .as_slice(),
        ),
        (
            4,
            br#"{"jsonrpc":"2.0","id":4,"method":"tracedecay/subscribe","params":{"projections":[{"kind":"diagnostics","revision":1,"unexpected":true}]}}"#
                .as_slice(),
        ),
    ] {
        session.handle_payload(request, id);
        let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
        assert_eq!(response["id"], id);
        assert_eq!(response["error"]["code"], -32602);
    }

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":5,"method":"tracedecay/context","params":{"kind":"diagnostics"}}"#,
        5,
    );
    assert!(session.drain_outbound().is_empty());

    session.flush_due(6);
    let response: Value = session
        .drain_outbound()
        .into_iter()
        .map(|message| serde_json::from_slice(&message).unwrap())
        .find(|message: &Value| message["id"] == 5)
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
        br#"{"jsonrpc":"2.0","id":10,"method":"tracedecay/context","params":{"kind":"diagnostics"}}"#,
        2,
    );
    assert!(session.drain_outbound().is_empty());
    session.flush_due(3);
    let projection: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(projection["id"], 10);

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context/expand","params":{"retrievalHandle":"rh_paged"}}"#,
        4,
    );
    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(response["result"]["rootUri"], "file:///root");
    assert_eq!(response["result"]["generation"], 1);
    assert_eq!(
        response["result"]["evidence"]["canonical"],
        "feedback-expand"
    );
    assert_eq!(response["result"]["nextRetrievalHandle"], "rh_next_page");

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

#[test]
fn context_expansion_requires_matching_session_currentness() {
    let mut session = session_with_context(PendingContext::default());
    initialize_context(&mut session, TRACEDECAY_CONTEXT_REVISION);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context/expand","params":{"retrievalHandle":"rh_current"}}"#,
        2,
    );
    let rejected: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(rejected["error"]["code"], -32603);
    assert_eq!(
        rejected["error"]["data"]["failureClass"],
        "invalid-context-expansion"
    );
}

#[test]
fn incompatible_context_revision_preserves_standard_lsp_and_stays_unavailable() {
    let mut session = session_with_context(PendingContext::default());
    let response = initialize_context(&mut session, TRACEDECAY_CONTEXT_REVISION.saturating_add(1));
    assert_eq!(
        response["result"]["capabilities"]["positionEncoding"],
        "utf-16"
    );
    assert!(
        response["result"]["capabilities"]
            .get("experimental")
            .is_none()
    );

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"diagnostics"}}"#,
        2,
    );
    let unavailable: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(unavailable["error"]["code"], -32601);
    assert_eq!(
        unavailable["error"]["data"]["reason"],
        "capabilityNotNegotiated"
    );
}

#[test]
fn cancelling_pending_context_request_cancels_owner_and_completes_correlation() {
    let context = Arc::new(PendingContext::default());
    let mut session = session_with_context(context.clone());
    initialize_context(&mut session, TRACEDECAY_CONTEXT_REVISION);

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"diagnostics"}}"#,
        2,
    );
    assert!(session.drain_outbound().is_empty());
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":2}}"#,
        3,
    );

    let cancelled: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(cancelled["id"], 2);
    assert_eq!(cancelled["error"]["code"], -32800);
    assert_eq!(context.cancellations.load(Ordering::SeqCst), 1);
}

#[test]
fn context_deadline_cancels_owner_and_returns_retriable_timeout() {
    let context = Arc::new(PendingContext::default());
    let mut session = session_with_context(context.clone());
    initialize_context(&mut session, TRACEDECAY_CONTEXT_REVISION);
    session.set_request_deadline_ms(1);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"diagnostics"}}"#,
        2,
    );
    assert!(session.drain_outbound().is_empty());

    session.flush_due(3);
    let timed_out: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(timed_out["id"], 2);
    assert_eq!(timed_out["error"]["code"], -32802);
    assert_eq!(timed_out["error"]["data"]["retriggerRequest"], true);
    assert_eq!(context.cancellations.load(Ordering::SeqCst), 1);
}

#[test]
fn shutdown_cancels_pending_context_work_before_entering_shutdown() {
    let context = Arc::new(PendingContext::default());
    let mut session = session_with_context(context.clone());
    initialize_context(&mut session, TRACEDECAY_CONTEXT_REVISION);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"diagnostics"}}"#,
        2,
    );
    assert!(session.drain_outbound().is_empty());

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":3,"method":"shutdown","params":null}"#,
        3,
    );
    let responses = session
        .drain_outbound()
        .into_iter()
        .map(|message| serde_json::from_slice::<Value>(&message).unwrap())
        .collect::<Vec<_>>();
    assert!(
        responses
            .iter()
            .any(|response| response["id"] == 2 && response["error"]["code"] == -32800)
    );
    assert!(
        responses
            .iter()
            .any(|response| response["id"] == 3 && response["result"].is_null())
    );
    assert_eq!(session.lifecycle(), SessionLifecycle::Shutdown);
    assert_eq!(context.cancellations.load(Ordering::SeqCst), 1);
}

struct IdentityDriftContext {
    requests: AtomicUsize,
}

impl ContextProjectionPort for IdentityDriftContext {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        vec![ContextProjectionRegistration {
            kind: ContextProjectionKind::diagnostics(),
            revision: TRACEDECAY_CONTEXT_REVISION,
        }]
    }

    fn snapshot(
        &self,
        root: &AdmittedRoot,
        _request_id: &LspRequestId,
        _request: &ContextProjectionRequest,
    ) -> ContextProjectionOutcome {
        let mut identity = fixture_projection_identity();
        if self.requests.fetch_add(1, Ordering::SeqCst) != 0 {
            identity.head_commit_id = "fedcba9876543210".to_owned();
        }
        ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
            root_uri: root.uri().to_owned(),
            document_uri: None,
            kind: ContextProjectionKind::diagnostics(),
            generation: 1,
            identity,
            freshness: ContextFreshness::Current,
            producer_state: ContextProducerState::Complete,
            coverage: ContextCoverage::Complete,
            revision: TRACEDECAY_CONTEXT_REVISION,
            items: Vec::new(),
            omitted_count: 0,
            omission_reasons: Vec::new(),
            retrieval_handle: None,
        })
    }
}

#[test]
fn context_projection_rejects_result_for_stale_open_document_content() {
    struct StaleDocumentContext;

    impl ContextProjectionPort for StaleDocumentContext {
        fn registrations(&self) -> Vec<ContextProjectionRegistration> {
            vec![ContextProjectionRegistration {
                kind: ContextProjectionKind::diagnostics(),
                revision: TRACEDECAY_CONTEXT_REVISION,
            }]
        }

        fn snapshot(
            &self,
            root: &AdmittedRoot,
            _request_id: &LspRequestId,
            request: &ContextProjectionRequest,
        ) -> ContextProjectionOutcome {
            let mut identity = fixture_projection_identity();
            identity.document_content_digest = Some(format!("sha256:{}", "d".repeat(64)));
            ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
                root_uri: root.uri().to_owned(),
                document_uri: request.document_uri.clone(),
                kind: ContextProjectionKind::diagnostics(),
                generation: 1,
                identity,
                freshness: ContextFreshness::Current,
                producer_state: ContextProducerState::Complete,
                coverage: ContextCoverage::Complete,
                revision: TRACEDECAY_CONTEXT_REVISION,
                items: Vec::new(),
                omitted_count: 0,
                omission_reasons: Vec::new(),
                retrieval_handle: None,
            })
        }
    }

    let mut session = session_with_context(Arc::new(StaleDocumentContext));
    initialize_context(&mut session, TRACEDECAY_CONTEXT_REVISION);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///root/a.rs","languageId":"rust","version":1,"text":"fn current_overlay() {}"}}}"#,
        2,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"diagnostics","documentUri":"file:///root/a.rs"}}"#,
        3,
    );

    let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
    assert_eq!(response["error"]["code"], -32603);
    assert_eq!(
        response["error"]["data"]["failureClass"],
        "invalid-context-projection"
    );
}

#[test]
fn equal_generation_cannot_replace_a_different_projection_identity() {
    let mut session = session_with_context(IdentityDriftContext {
        requests: AtomicUsize::new(0),
    });
    initialize_context(&mut session, TRACEDECAY_CONTEXT_REVISION);

    for id in [2, 3] {
        session.handle_payload(
            &serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tracedecay/context",
                "params": { "kind": "diagnostics" }
            }))
            .unwrap(),
            id,
        );
        let response: Value = serde_json::from_slice(&session.drain_outbound()[0]).unwrap();
        if id == 2 {
            assert_eq!(response["result"]["generation"], 1);
        } else {
            assert_eq!(response["error"]["code"], -32802);
            assert_eq!(
                response["error"]["data"]["failureClass"],
                "superseded-generation"
            );
        }
    }
}

struct EqualGenerationChangeContext {
    emitted: AtomicUsize,
}

impl ContextProjectionPort for EqualGenerationChangeContext {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        vec![ContextProjectionRegistration {
            kind: ContextProjectionKind::diagnostics(),
            revision: TRACEDECAY_CONTEXT_REVISION,
        }]
    }

    fn snapshot(
        &self,
        root: &AdmittedRoot,
        _request_id: &LspRequestId,
        _request: &ContextProjectionRequest,
    ) -> ContextProjectionOutcome {
        ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
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
        })
    }

    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        _subscriptions: &std::collections::BTreeSet<ContextProjectionRegistration>,
        _maximum: usize,
    ) -> Vec<ContextProjectionChange> {
        if self.emitted.fetch_add(1, Ordering::SeqCst) != 0 {
            return Vec::new();
        }
        let mut identity = fixture_projection_identity();
        identity.head_commit_id = "fedcba9876543210".to_owned();
        vec![ContextProjectionChange {
            root_uri: root.uri().to_owned(),
            document_uri: None,
            kind: ContextProjectionKind::diagnostics(),
            generation: 1,
            identity,
            freshness: ContextFreshness::Current,
            producer_state: ContextProducerState::Complete,
            coverage: ContextCoverage::Complete,
            revision: TRACEDECAY_CONTEXT_REVISION,
            retrieval_handle: None,
        }]
    }
}

#[test]
fn equal_generation_identity_change_clears_subscription_currentness() {
    let mut session = session_with_context(EqualGenerationChangeContext {
        emitted: AtomicUsize::new(0),
    });
    initialize_context(&mut session, TRACEDECAY_CONTEXT_REVISION);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"diagnostics"}}"#,
        2,
    );
    session.drain_outbound();

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":3,"method":"tracedecay/subscribe","params":{"projections":[{"kind":"diagnostics","revision":1}]}}"#,
        3,
    );
    let messages = session
        .drain_outbound()
        .into_iter()
        .map(|message| serde_json::from_slice::<Value>(&message).unwrap())
        .collect::<Vec<_>>();
    let change = messages
        .iter()
        .find(|message| message["method"] == "tracedecay/contextChanged")
        .expect("identity change notification");
    assert_eq!(
        change["params"]["identity"]["headCommitId"],
        "0123456789abcdef"
    );
    assert_eq!(change["params"]["coverage"], "unavailable");
    assert_eq!(change["params"]["producerState"], "unavailable");
}

struct SameGenerationFeedbackChangeContext {
    emitted: AtomicUsize,
}

impl ContextProjectionPort for SameGenerationFeedbackChangeContext {
    fn registrations(&self) -> Vec<ContextProjectionRegistration> {
        vec![ContextProjectionRegistration {
            kind: ContextProjectionKind::diagnostics(),
            revision: TRACEDECAY_CONTEXT_REVISION,
        }]
    }

    fn snapshot(
        &self,
        root: &AdmittedRoot,
        _request_id: &LspRequestId,
        _request: &ContextProjectionRequest,
    ) -> ContextProjectionOutcome {
        ContextProjectionOutcome::Ready(ContextProjectionEnvelope {
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
        })
    }

    fn poll_changes(
        &self,
        root: &AdmittedRoot,
        _subscriptions: &std::collections::BTreeSet<ContextProjectionRegistration>,
        _maximum: usize,
    ) -> Vec<ContextProjectionChange> {
        if self.emitted.fetch_add(1, Ordering::SeqCst) != 0 {
            return Vec::new();
        }
        vec![ContextProjectionChange {
            root_uri: root.uri().to_owned(),
            document_uri: None,
            kind: ContextProjectionKind::diagnostics(),
            generation: 1,
            identity: fixture_projection_identity(),
            freshness: ContextFreshness::Current,
            producer_state: ContextProducerState::Complete,
            coverage: ContextCoverage::Complete,
            revision: TRACEDECAY_CONTEXT_REVISION,
            retrieval_handle: Some("rh_feedback_2".to_owned()),
        }]
    }
}

#[test]
fn same_generation_feedback_revision_notifies_subscribers() {
    let mut session = session_with_context(SameGenerationFeedbackChangeContext {
        emitted: AtomicUsize::new(0),
    });
    initialize_context(&mut session, TRACEDECAY_CONTEXT_REVISION);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":2,"method":"tracedecay/context","params":{"kind":"diagnostics"}}"#,
        2,
    );
    session.drain_outbound();

    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":3,"method":"tracedecay/subscribe","params":{"projections":[{"kind":"diagnostics","revision":1}]}}"#,
        3,
    );
    let messages = session
        .drain_outbound()
        .into_iter()
        .map(|message| serde_json::from_slice::<Value>(&message).unwrap())
        .collect::<Vec<_>>();
    let change = messages
        .iter()
        .find(|message| message["method"] == "tracedecay/contextChanged")
        .expect("new feedback result in the same code generation must notify");
    assert_eq!(change["params"]["retrievalHandle"], "rh_feedback_2");
}
