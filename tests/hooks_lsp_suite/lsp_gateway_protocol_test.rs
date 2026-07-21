use serde_json::{Value, json};
use tracedecay::daemon::lsp_gateway::{
    AdmittedRoot, ClientCapabilities, DaemonLspGateway, DaemonLspProtocolSession,
    FeedbackCyclePort, FeedbackCycleRequest, FeedbackCycleResponse, GatewayCapabilities,
    SemanticProviderPort, UnavailableDiagnosticSnapshotProvider, UpstreamCapabilities,
    negotiate_capabilities,
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
