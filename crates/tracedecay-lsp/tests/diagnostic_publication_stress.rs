use std::sync::{Arc, Mutex};

use serde_json::Value;
use tracedecay_lsp::{
    AdmittedRoot, ClientCapabilities, DaemonLspGateway, DaemonLspProtocolSession,
    DaemonLspSessionTransport, DiagnosticSeverity, DiagnosticSnapshotOutcome,
    DiagnosticSnapshotPort, FeedbackCyclePort, FeedbackCycleRequest, FeedbackCycleResponse,
    FramePoll, GatewayCapabilities, GatewayDiagnostic, GenerationDiagnostics, LspPosition,
    LspRange, MAX_PUBLICATION_BYTES, SemanticProviderPort, UpstreamCapabilities,
    negotiate_capabilities,
};

const ROOT_URI: &str = "file:///stress";

#[derive(Default)]
struct Feedback;

impl FeedbackCyclePort for Feedback {
    fn request_feedback_cycle(&self, _request: FeedbackCycleRequest) -> FeedbackCycleResponse {
        FeedbackCycleResponse::Accepted
    }
}

#[derive(Default)]
struct Semantics;

impl SemanticProviderPort for Semantics {}

#[derive(Clone)]
struct SnapshotProvider {
    snapshot: Arc<Mutex<(u64, String)>>,
}

impl SnapshotProvider {
    fn new(generation: u64, message: &str) -> Self {
        Self {
            snapshot: Arc::new(Mutex::new((generation, message.to_owned()))),
        }
    }

    fn set(&self, generation: u64, message: &str) {
        *self.snapshot.lock().expect("snapshot lock") = (generation, message.to_owned());
    }
}

impl DiagnosticSnapshotPort for SnapshotProvider {
    fn document_diagnostics(
        &self,
        _root: &AdmittedRoot,
        uri: &str,
        _overlay: Option<&tracedecay_lsp::OverlaySnapshot>,
    ) -> DiagnosticSnapshotOutcome {
        let (generation, message) = self.snapshot.lock().expect("snapshot lock").clone();
        DiagnosticSnapshotOutcome::Ready {
            diagnostics: GenerationDiagnostics {
                generation,
                upstream: Vec::new(),
                tracedecay: vec![GatewayDiagnostic {
                    uri: uri.to_owned(),
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
                    code: Some("stress".to_owned()),
                    code_description_uri: None,
                    message,
                    source: tracedecay_lsp::DiagnosticSource::TraceDecay,
                    related_information: Vec::new(),
                    data: None,
                }],
            },
            completed_operation_id: None,
        }
    }
}

type StressSession = DaemonLspProtocolSession<Feedback, Semantics, SnapshotProvider>;

fn session(provider: SnapshotProvider) -> StressSession {
    let gateway_capabilities = GatewayCapabilities::default();
    let upstream_capabilities = UpstreamCapabilities {
        supports_diagnostics: true,
        semantic: Default::default(),
    };
    let effective = negotiate_capabilities(
        &ClientCapabilities {
            supports_versioned_publish_diagnostics: true,
            ..ClientCapabilities::default()
        },
        &gateway_capabilities,
        &upstream_capabilities,
    );
    let mut session = DaemonLspProtocolSession::new(
        DaemonLspGateway::with_semantic_provider(
            AdmittedRoot::new(ROOT_URI),
            effective,
            Feedback,
            Semantics,
        ),
        gateway_capabilities,
        upstream_capabilities,
        provider,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"rootUri":"file:///stress","capabilities":{"general":{"positionEncodings":["utf-16"]},"textDocument":{"publishDiagnostics":{"versionSupport":true,"dataSupport":true}}}}}"#,
        0,
    );
    session.drain_outbound();
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#,
        1,
    );
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{"textDocument":{"uri":"file:///stress/src/lib.rs","languageId":"rust","version":1,"text":"x"}}}"#,
        2,
    );
    session.drain_outbound();
    session
}

fn save(session: &mut StressSession, now_ms: u64) {
    session.handle_payload(
        br#"{"jsonrpc":"2.0","method":"textDocument/didSave","params":{"textDocument":{"uri":"file:///stress/src/lib.rs"}}}"#,
        now_ms,
    );
}

fn publications(frames: Vec<Vec<u8>>) -> Vec<Value> {
    frames
        .into_iter()
        .map(|frame| serde_json::from_slice::<Value>(&frame).expect("JSON-RPC frame"))
        .filter(|frame| frame["method"] == "textDocument/publishDiagnostics")
        .collect()
}

fn message(frame: &Value) -> &str {
    frame["params"]["diagnostics"][0]["message"]
        .as_str()
        .expect("diagnostic message")
}

#[test]
fn identical_snapshots_coalesce_and_distinct_same_generation_publishes() {
    let provider = SnapshotProvider::new(7, "first");
    let mut session = session(provider.clone());

    for now_ms in 3..1_003 {
        save(&mut session, now_ms);
    }
    let identical = publications(session.drain_outbound());
    assert_eq!(identical.len(), 1, "1000 identical events must emit once");
    assert_eq!(message(&identical[0]), "first");

    provider.set(7, "changed");
    save(&mut session, 1_003);
    let changed = publications(session.drain_outbound());
    assert_eq!(
        changed.len(),
        1,
        "changed payload at the same generation remains observable"
    );
    assert_eq!(message(&changed[0]), "changed");
}

#[test]
fn stale_snapshots_drop_and_distinct_generations_remain_ordered() {
    let provider = SnapshotProvider::new(10, "ten");
    let mut session = session(provider.clone());
    save(&mut session, 3);
    let ten = publications(session.drain_outbound());
    assert_eq!(message(&ten[0]), "ten");

    provider.set(9, "stale");
    save(&mut session, 4);
    assert!(publications(session.drain_outbound()).is_empty());

    provider.set(11, "eleven");
    save(&mut session, 5);
    provider.set(12, "twelve");
    save(&mut session, 6);
    let ordered = publications(session.drain_outbound());
    assert_eq!(
        ordered.len(),
        1,
        "backpressure coalesces to the newest event"
    );
    assert_eq!(message(&ordered[0]), "twelve");
    assert_eq!(ordered[0]["params"]["version"], 1);
}

#[test]
fn publication_rate_and_queue_memory_stay_bounded_under_backpressure() {
    let provider = SnapshotProvider::new(1, "generation-1");
    let mut session = session(provider.clone());

    let attempted_events = 10_000_u64;
    for generation in 1..=attempted_events {
        provider.set(generation, &format!("generation-{generation}"));
        save(&mut session, generation + 2);
    }
    let queued = session.drain_outbound();
    let queued_bytes = queued.iter().map(Vec::len).sum::<usize>();
    let emitted = publications(queued);

    assert_eq!(emitted.len(), 1);
    assert_eq!(message(&emitted[0]), "generation-10000");
    assert!(queued_bytes <= MAX_PUBLICATION_BYTES);
    assert_eq!(attempted_events / emitted.len() as u64, 10_000);
}

#[test]
fn disconnect_and_expiry_drop_retained_publications_without_flooding_client() {
    let provider = SnapshotProvider::new(1, "queued");
    let mut session = session(provider);
    save(&mut session, 3);

    let first = session.poll_outbound().expect("held publication").to_vec();
    assert_eq!(
        serde_json::from_slice::<Value>(&first).expect("publication")["method"],
        "textDocument/publishDiagnostics"
    );
    session.detach().expect("detach");
    session.expire();

    assert!(session.poll_outbound().is_none());
    assert!(session.drain_outbound().is_empty());
    let mut transport = tracedecay_lsp::DaemonLspProtocolTransport::new(session);
    assert_eq!(transport.poll_daemon_frame(), Ok(FramePoll::Closed));
}
