use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracedecay_daemon_protocol::MAX_LSP_FRAME_BYTES;
use tracedecay_lsp::analyzer::adapters::{DiagnosticMode, LspAdapterDefinition};
use tracedecay_lsp::analyzer::broker::DiagnosticBroker;
use tracedecay_lsp::analyzer::client::LspRefreshTimeouts;
use tracedecay_lsp::{
    AdmittedRoot, LspRequestId, SemanticCapability, SemanticProviderOutcome, SemanticRequest,
    SemanticResponse,
};

use super::{ProductionSemanticAuthorities, production_semantic_authorities};

const TEST_BOUND: Duration = Duration::from_secs(10);

// Only the external analyzer is a fixture. Requests pass through the real
// production route factory, bounded adapter, broker, and stdio client.
const ANALYZER: &str = r#"
import json
import socket
import sys

language, supported, port = sys.argv[1:]

def receive():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        headers[name.lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))

def send(value):
    body = json.dumps(value).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n" + body)
    sys.stdout.buffer.flush()

while True:
    message = receive()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {
            "workspaceSymbolProvider": supported == "true", "documentSymbolProvider": True
        }}})
    elif method == "workspace/symbol" and supported != "true":
        send({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32601, "message": "unsupported"}})
    elif method in ("workspace/symbol", "textDocument/documentSymbol"):
        with socket.create_connection(("127.0.0.1", int(port))) as control:
            control.sendall(json.dumps(message).encode("utf-8") + b"\n")
            response = control.makefile("rb").readline()
            if not response:
                break
            result = json.loads(response)
            result.update({"jsonrpc": "2.0", "id": message["id"]})
            send(result)
"#;

struct AnalyzerControl(TcpListener);

struct PendingAnalyzerRequest {
    stream: BufReader<TcpStream>,
    request: Value,
}

impl AnalyzerControl {
    async fn next(&self) -> PendingAnalyzerRequest {
        tokio::time::timeout(TEST_BOUND, async {
            let (stream, _) = self.0.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            assert_ne!(stream.read_line(&mut line).await.unwrap(), 0);
            PendingAnalyzerRequest {
                stream,
                request: serde_json::from_str(&line).unwrap(),
            }
        })
        .await
        .expect("analyzer must receive its routed semantic request")
    }
}

impl PendingAnalyzerRequest {
    async fn respond(mut self, response: Value) {
        let mut line = serde_json::to_vec(&response).unwrap();
        line.push(b'\n');
        tokio::time::timeout(TEST_BOUND, self.stream.get_mut().write_all(&line))
            .await
            .expect("analyzer response control must remain writable")
            .unwrap();
    }

    async fn assert_stopped(mut self) {
        let mut byte = [0_u8];
        let bytes = tokio::time::timeout(TEST_BOUND, self.stream.read(&mut byte))
            .await
            .expect("cancelled analyzer work must release its process")
            .unwrap();
        assert_eq!(bytes, 0);
    }
}

struct Harness {
    _project: tempfile::TempDir,
    root: AdmittedRoot,
    production: ProductionSemanticAuthorities,
    controls: Vec<AnalyzerControl>,
}

impl Harness {
    async fn new(languages: &[(&str, bool)]) -> Self {
        let project = tempfile::tempdir().unwrap();
        let script = project.path().join("analyzer.py");
        std::fs::write(&script, ANALYZER).unwrap();
        let mut adapters = Vec::new();
        let mut controls = Vec::new();
        for (language, supported) in languages {
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let port = listener.local_addr().unwrap().port();
            adapters.push(LspAdapterDefinition {
                language: (*language).to_owned(),
                language_id: (*language).to_owned(),
                command: python().to_owned(),
                args: vec![
                    script.display().to_string(),
                    (*language).to_owned(),
                    supported.to_string(),
                    port.to_string(),
                ],
                extensions: vec![(*language).to_owned(), "shared".to_owned()],
                root_markers: Vec::new(),
                install_options: Vec::new(),
                diagnostics: DiagnosticMode::Push,
            });
            std::fs::write(project.path().join(format!("file.{language}")), "symbol").unwrap();
            controls.push(AnalyzerControl(listener));
        }
        let root = AdmittedRoot::new(
            url::Url::from_directory_path(project.path())
                .unwrap()
                .to_string(),
        );
        let production = production_semantic_authorities(
            Arc::new(Mutex::new(DiagnosticBroker::new_for_test(
                project.path(),
                adapters,
            ))),
            &languages
                .iter()
                .map(|(language, _)| (*language).to_owned())
                .collect::<Vec<_>>(),
            project.path().to_owned(),
            root.uri(),
            LspRefreshTimeouts::new(
                TEST_BOUND,
                TEST_BOUND,
                TEST_BOUND,
                Duration::from_millis(50),
            ),
        )
        .await
        .unwrap();
        let capabilities = production
            .upstream_capability_initializer
            .initialize_upstream_capabilities()
            .await
            .unwrap();
        assert_eq!(
            capabilities
                .semantic
                .contains(&SemanticCapability::WorkspaceSymbol),
            languages.iter().any(|(_, supported)| *supported)
        );
        Self {
            _project: project,
            root,
            production,
            controls,
        }
    }

    fn symbol(&self, name: &str, extension: &str) -> Value {
        json!({
            "name": name, "kind": 12,
            "location": {
                "uri": format!("{}file.{extension}", self.root.uri()),
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}
            }
        })
    }

    fn request(
        &self,
        id: i64,
        request: &SemanticRequest,
        deadline: Instant,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        self.production.semantics.request_with_document(
            &self.root,
            &LspRequestId::Number(id),
            request,
            None,
            Some(deadline),
        )
    }

    async fn complete(
        &self,
        id: i64,
        request: &SemanticRequest,
        deadline: Instant,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        tokio::time::timeout(TEST_BOUND, async {
            loop {
                match self.request(id, request, deadline) {
                    SemanticProviderOutcome::Pending => tokio::task::yield_now().await,
                    terminal => return terminal,
                }
            }
        })
        .await
        .expect("production semantic operation must settle")
    }
}

fn python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else if Path::new("/usr/bin/python3").is_file() {
        "/usr/bin/python3"
    } else {
        "python3"
    }
}

fn symbols_request() -> SemanticRequest {
    SemanticRequest::WorkspaceSymbols {
        query: "needle".to_owned(),
    }
}

#[tokio::test]
async fn advertised_workspace_symbols_route_across_languages_in_configured_order() {
    let harness = Harness::new(&[("alpha", true), ("beta", true), ("gamma", false)]).await;
    let request = symbols_request();
    let deadline = Instant::now() + TEST_BOUND;
    assert_eq!(
        harness.request(1, &request, deadline),
        SemanticProviderOutcome::Pending
    );
    let (alpha, beta) = tokio::join!(harness.controls[0].next(), harness.controls[1].next());
    assert_eq!(alpha.request["params"]["query"], "needle");
    assert_eq!(beta.request["params"]["query"], "needle");
    beta.respond(json!({"result": [harness.symbol("beta", "beta")]}))
        .await;

    // The later language must finish first. A document request acquiring its
    // retained client proves the workspace response was consumed, without a
    // wall-clock delay or an assumed executor scheduling order.
    let document = SemanticRequest::DocumentSymbols {
        document_uri: format!("{}file.beta", harness.root.uri()),
    };
    assert_eq!(
        harness.request(2, &document, deadline),
        SemanticProviderOutcome::Pending
    );
    let beta_document = harness.controls[1].next().await;
    assert_eq!(
        beta_document.request["method"],
        "textDocument/documentSymbol"
    );
    beta_document.respond(json!({"result": []})).await;
    assert!(matches!(
        harness.complete(2, &document, deadline).await,
        SemanticProviderOutcome::Complete(SemanticResponse::DocumentSymbols(_))
    ));
    assert_eq!(
        harness.request(1, &request, deadline),
        SemanticProviderOutcome::Pending
    );
    alpha
        .respond(json!({"result": [harness.symbol("alpha", "alpha")]}))
        .await;
    let SemanticProviderOutcome::Complete(SemanticResponse::WorkspaceSymbols(symbols)) =
        harness.complete(1, &request, deadline).await
    else {
        panic!("advertised workspace symbols must return the real language results");
    };
    assert_eq!(
        symbols
            .iter()
            .map(|symbol| symbol.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );

    for extension in ["unknown", "shared"] {
        let document = SemanticRequest::DocumentSymbols {
            document_uri: format!("{}file.{extension}", harness.root.uri()),
        };
        assert_eq!(
            harness.complete(3, &document, deadline).await,
            SemanticProviderOutcome::Unavailable,
            "workspace routing must not open unknown or ambiguous documents in every analyzer"
        );
    }
}

#[tokio::test]
async fn workspace_symbol_failure_cancels_pending_language_work() {
    for code in [-32603, -32601] {
        let harness = Harness::new(&[("alpha", true), ("beta", true)]).await;
        let request = symbols_request();
        let deadline = Instant::now() + TEST_BOUND;
        assert_eq!(
            harness.request(1, &request, deadline),
            SemanticProviderOutcome::Pending
        );
        let (alpha, beta) = tokio::join!(harness.controls[0].next(), harness.controls[1].next());
        alpha
            .respond(json!({"error": {"code": code, "message": "fixture failure"}}))
            .await;
        let outcome = harness.complete(1, &request, deadline).await;
        if code == -32603 {
            assert!(matches!(
                outcome,
                SemanticProviderOutcome::Partial { coverage, .. } if coverage == "analyzer-remote-error"
            ));
        } else {
            assert_eq!(outcome, SemanticProviderOutcome::Unavailable);
        }
        beta.assert_stopped().await;
        assert!(
            !harness
                .production
                .cancellation
                .cancel_request(&harness.root, &LspRequestId::Number(1))
        );
    }
}

#[tokio::test]
async fn workspace_symbols_preserve_deadline_and_cancel_all_languages() {
    let harness = Harness::new(&[("alpha", true), ("beta", true)]).await;
    let request = symbols_request();
    let expired = Instant::now();
    assert!(matches!(
        harness.complete(1, &request, expired).await,
        SemanticProviderOutcome::Partial { coverage, .. } if coverage == "analyzer-timed-out"
    ));
    let deadline = Instant::now() + TEST_BOUND;
    assert_eq!(
        harness.request(2, &request, deadline),
        SemanticProviderOutcome::Pending
    );
    let (alpha, beta) = tokio::join!(harness.controls[0].next(), harness.controls[1].next());
    assert!(
        harness
            .production
            .cancellation
            .cancel_request(&harness.root, &LspRequestId::Number(2))
    );
    tokio::join!(alpha.assert_stopped(), beta.assert_stopped());
    assert!(
        !harness
            .production
            .cancellation
            .cancel_request(&harness.root, &LspRequestId::Number(2))
    );
}

#[tokio::test]
async fn workspace_symbol_aggregation_respects_the_existing_frame_limit() {
    let harness = Harness::new(&[("alpha", true), ("beta", true)]).await;
    let request = symbols_request();
    let deadline = Instant::now() + TEST_BOUND;
    assert_eq!(
        harness.request(1, &request, deadline),
        SemanticProviderOutcome::Pending
    );
    let (alpha, beta) = tokio::join!(harness.controls[0].next(), harness.controls[1].next());
    let name = "x".repeat(MAX_LSP_FRAME_BYTES / 2);
    alpha
        .respond(json!({"result": [harness.symbol(&name, "alpha")]}))
        .await;
    beta.respond(json!({"result": [harness.symbol(&name, "beta")]}))
        .await;
    assert!(matches!(
        harness.complete(1, &request, deadline).await,
        SemanticProviderOutcome::Partial { coverage, .. }
            if coverage == "workspace-symbol-response-too-large"
    ));
}

#[tokio::test]
async fn workspace_symbols_without_any_support_remain_unavailable() {
    let harness = Harness::new(&[("alpha", false)]).await;
    assert_eq!(
        harness
            .complete(1, &symbols_request(), Instant::now() + TEST_BOUND)
            .await,
        SemanticProviderOutcome::Unavailable
    );
}
