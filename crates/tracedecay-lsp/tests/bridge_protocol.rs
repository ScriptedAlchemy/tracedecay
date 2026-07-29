use serde_json::{Value, json};
use tracedecay_lsp::{
    AdmittedRoot, AnalyzerEvent, AnalyzerSemanticAdapter, AnalyzerState, ClientCapabilities,
    ContextProjectionKind, ContextProjectionOutcome, ContextProjectionPort,
    ContextProjectionRegistration, ContextProjectionRequest, DaemonLspProtocolSession,
    DaemonLspProtocolTransport, DaemonLspSessionTransport, FeedbackCyclePort, FeedbackCycleRequest,
    FeedbackCycleResponse, FramePoll, FrameSend, GatewayCapabilities, LspLocation, LspPosition,
    LspRange, LspRequestId, SemanticCapability, SemanticProviderOutcome, SemanticProviderPort,
    TRACEDECAY_CONTEXT_REVISION, UnavailableDiagnosticSnapshotProvider, UpstreamCapabilities,
    negotiate_capabilities,
};

const ROOT_URI: &str = "file:///workspace";
const DOCUMENT_URI: &str = "file:///workspace/src/lib.rs";

type ProtocolSession<S> =
    DaemonLspProtocolSession<Feedback, S, UnavailableDiagnosticSnapshotProvider>;

struct Feedback;

impl FeedbackCyclePort for Feedback {
    fn request_feedback_cycle(&self, _request: FeedbackCycleRequest) -> FeedbackCycleResponse {
        FeedbackCycleResponse::Accepted
    }
}

struct AnalyzerFailure;

impl SemanticProviderPort for AnalyzerFailure {
    fn definition(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        assert_eq!(root.uri(), ROOT_URI);
        assert_eq!(document_uri, DOCUMENT_URI);
        SemanticProviderOutcome::Partial {
            value: Vec::new(),
            coverage: AnalyzerEvent::StartupFailed
                .coverage_token()
                .expect("startup failure has a stable token")
                .to_owned(),
            detail: AnalyzerEvent::StartupFailed
                .failure_detail()
                .map(str::to_owned),
        }
    }
}

struct UnavailableAnalyzer;

impl SemanticProviderPort for UnavailableAnalyzer {
    fn definition(
        &self,
        _root: &AdmittedRoot,
        _document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        panic!("a starting analyzer must not receive semantic requests")
    }
}

struct GraphEvidence;

impl SemanticProviderPort for GraphEvidence {
    fn definition(
        &self,
        root: &AdmittedRoot,
        document_uri: &str,
        _position: LspPosition,
    ) -> SemanticProviderOutcome<Vec<LspLocation>> {
        assert_eq!(root.uri(), ROOT_URI);
        assert_eq!(document_uri, DOCUMENT_URI);
        SemanticProviderOutcome::Complete(vec![LspLocation {
            uri: DOCUMENT_URI.to_owned(),
            range: LspRange {
                start: LspPosition {
                    line: 2,
                    character: 4,
                },
                end: LspPosition {
                    line: 2,
                    character: 10,
                },
            },
        }])
    }
}

struct Context;

impl ContextProjectionPort for Context {
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
        ContextProjectionOutcome::Unsupported
    }
}

fn protocol_session<S>(semantic_provider: S) -> ProtocolSession<S>
where
    S: SemanticProviderPort,
{
    let gateway_capabilities = GatewayCapabilities::default();
    let upstream_capabilities = UpstreamCapabilities {
        supports_diagnostics: true,
        semantic: [SemanticCapability::Definition].into_iter().collect(),
    };
    let initial_capabilities = negotiate_capabilities(
        &ClientCapabilities {
            semantic: [SemanticCapability::Definition].into_iter().collect(),
            ..ClientCapabilities::default()
        },
        &gateway_capabilities,
        &upstream_capabilities,
    );
    DaemonLspProtocolSession::from_ports(
        AdmittedRoot::new(ROOT_URI),
        initial_capabilities,
        gateway_capabilities,
        upstream_capabilities,
        Feedback,
        semantic_provider,
        UnavailableDiagnosticSnapshotProvider,
    )
}

fn context_session() -> ProtocolSession<GraphEvidence> {
    let mut gateway_capabilities = GatewayCapabilities::default();
    gateway_capabilities.context_projections.insert(
        ContextProjectionKind::diagnostics(),
        TRACEDECAY_CONTEXT_REVISION,
    );
    let upstream_capabilities = UpstreamCapabilities {
        supports_diagnostics: true,
        semantic: [SemanticCapability::Definition].into_iter().collect(),
    };
    let initial_capabilities = negotiate_capabilities(
        &ClientCapabilities::default(),
        &gateway_capabilities,
        &upstream_capabilities,
    );
    DaemonLspProtocolSession::from_ports(
        AdmittedRoot::new(ROOT_URI),
        initial_capabilities,
        gateway_capabilities,
        upstream_capabilities,
        Feedback,
        GraphEvidence,
        UnavailableDiagnosticSnapshotProvider,
    )
    .with_context_projection_port(Context)
}

fn initialize<S>(session: &mut ProtocolSession<S>, experimental_revision: Option<u32>) -> Value
where
    S: SemanticProviderPort,
{
    let mut capabilities = json!({
        "general": { "positionEncodings": ["utf-16"] },
        "textDocument": { "definition": {} },
    });
    if let Some(revision) = experimental_revision {
        capabilities["experimental"] = json!({
            "tracedecay": {
                "revision": revision,
                "opaqueExpansion": true,
                "projections": [{
                    "kind": "diagnostics",
                    "revision": TRACEDECAY_CONTEXT_REVISION,
                }],
            },
        });
    }
    let request = json!({
        "jsonrpc": "2.0",
        "id": "initialize",
        "method": "initialize",
        "params": {
            "rootUri": ROOT_URI,
            "capabilities": capabilities,
        },
    });
    session.handle_payload(
        &serde_json::to_vec(&request).expect("serialize initialize request"),
        0,
    );
    let response = next_response(session);
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    response
}

fn next_response<S>(session: &mut ProtocolSession<S>) -> Value
where
    S: SemanticProviderPort,
{
    let messages = session.drain_outbound();
    assert_eq!(messages.len(), 1);
    serde_json::from_slice(&messages[0]).expect("valid JSON-RPC response")
}

fn transport_frame<S>(
    transport: &mut DaemonLspProtocolTransport<Feedback, S, UnavailableDiagnosticSnapshotProvider>,
) -> Vec<u8>
where
    S: SemanticProviderPort,
{
    match transport.poll_daemon_frame().expect("poll transport") {
        FramePoll::Frame(frame) => frame,
        outcome => panic!("expected an outbound frame, got {outcome:?}"),
    }
}

#[test]
fn direct_and_transport_paths_are_byte_equivalent() {
    let initialize = br#"{"jsonrpc":"2.0","id":"initialize","method":"initialize","params":{"rootUri":"file:///workspace","capabilities":{"general":{"positionEncodings":["utf-16"]},"textDocument":{"definition":{}}}}}"#;
    let initialized = br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#;
    let definition = br#"{"jsonrpc":"2.0","id":"definition","method":"textDocument/definition","params":{"textDocument":{"uri":"file:///workspace/src/lib.rs"},"position":{"line":0,"character":0}}}"#;

    let mut direct = protocol_session(AnalyzerFailure);
    let mut transport = DaemonLspProtocolTransport::new(protocol_session(AnalyzerFailure));
    direct.handle_payload(initialize, 0);
    assert_eq!(
        transport.try_send_client_frame(initialize).unwrap(),
        FrameSend::Sent
    );
    let direct_initialize = direct.poll_outbound().unwrap().to_vec();
    let transport_initialize = transport_frame(&mut transport);
    assert_eq!(direct_initialize, transport_initialize);
    assert!(direct.acknowledge_outbound());
    transport.acknowledge_daemon_frame().unwrap();

    direct.handle_payload(initialized, 1);
    assert_eq!(
        transport.try_send_client_frame(initialized).unwrap(),
        FrameSend::Sent
    );
    direct.handle_payload(definition, 2);
    assert_eq!(
        transport.try_send_client_frame(definition).unwrap(),
        FrameSend::Sent
    );
    let direct_definition = direct.poll_outbound().unwrap().to_vec();
    let transport_definition = transport_frame(&mut transport);
    assert_eq!(direct_definition, transport_definition);

    let response: Value = serde_json::from_slice(&direct_definition).unwrap();
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["error"]["data"]["retriggerRequest"], true);
    assert_eq!(
        response["error"]["data"]["detail"],
        "Analyzer failed to start."
    );
}

#[test]
fn direct_session_preserves_json_rpc_and_typed_analyzer_failure() {
    let mut session = protocol_session(AnalyzerFailure);
    let initialize_response = initialize(&mut session, None);
    assert_eq!(initialize_response["jsonrpc"], "2.0");
    assert_eq!(initialize_response["id"], "initialize");
    assert_eq!(
        initialize_response["result"]["capabilities"]["definitionProvider"],
        true
    );

    let request = json!({
        "jsonrpc": "2.0",
        "id": "definition",
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": DOCUMENT_URI },
            "position": { "line": 0, "character": 0 },
        },
    });
    session.handle_payload(
        &serde_json::to_vec(&request).expect("serialize definition request"),
        2,
    );
    let response = next_response(&mut session);
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], "definition");
    assert_eq!(response["error"]["code"], -32802);
    assert_eq!(response["error"]["message"], "Server cancelled request");
    assert_eq!(response["error"]["data"]["retriggerRequest"], true);
    assert_eq!(
        response["error"]["data"]["coverage"],
        "analyzer-start-failed"
    );
    assert_eq!(
        response["error"]["data"]["detail"],
        "Analyzer failed to start."
    );
}

#[test]
fn versioned_context_negotiation_preserves_standard_lsp_compatibility() {
    let mut session = context_session();
    let current = initialize(&mut session, Some(TRACEDECAY_CONTEXT_REVISION));
    assert_eq!(
        current["result"]["capabilities"]["experimental"]["tracedecay"]["revision"],
        TRACEDECAY_CONTEXT_REVISION
    );
    assert_eq!(
        current["result"]["capabilities"]["experimental"]["tracedecay"]["projections"][0]["kind"],
        "diagnostics"
    );
    assert_eq!(
        current["result"]["capabilities"]["definitionProvider"],
        true
    );

    let mut future = context_session();
    let future_response = initialize(&mut future, Some(TRACEDECAY_CONTEXT_REVISION + 1));
    assert_eq!(
        future_response["result"]["capabilities"]["definitionProvider"],
        true
    );
    assert!(
        future_response["result"]["capabilities"]
            .get("experimental")
            .is_none(),
        "an incompatible extension revision must not disable standard LSP"
    );
}

#[test]
fn starting_analyzer_uses_only_admitted_project_graph_evidence() {
    let semantic_provider =
        AnalyzerSemanticAdapter::new(AnalyzerState::Starting, UnavailableAnalyzer, GraphEvidence);
    let mut session = protocol_session(semantic_provider);
    initialize(&mut session, None);

    let request = json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": DOCUMENT_URI },
            "position": { "line": 0, "character": 0 },
        },
    });
    session.handle_payload(
        &serde_json::to_vec(&request).expect("serialize definition request"),
        2,
    );
    let response = next_response(&mut session);
    assert_eq!(response["result"][0]["uri"], DOCUMENT_URI);
    assert_eq!(response["result"][0]["range"]["start"]["line"], 2);

    let outside_root = json!({
        "jsonrpc": "2.0",
        "id": 8,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": "file:///other/src/lib.rs" },
            "position": { "line": 0, "character": 0 },
        },
    });
    session.handle_payload(
        &serde_json::to_vec(&outside_root).expect("serialize outside-root request"),
        3,
    );
    let response = next_response(&mut session);
    assert_eq!(response["error"]["code"], -32601);
    assert_eq!(response["error"]["data"]["reason"], "outsideAdmittedRoot");
}
