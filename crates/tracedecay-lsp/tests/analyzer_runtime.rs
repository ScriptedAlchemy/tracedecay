#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tracedecay_lsp::analyzer as lsp;
use tracedecay_lsp::{
    AdmittedRoot, AnalyzerCancellationPort, AnalyzerEvent, AnalyzerState, AnalyzerSupervisor,
    AnalyzerTransitionError, LspRequestId, SemanticProviderOutcome, SemanticProviderPort,
    SemanticRequest, SemanticResponse,
};

const FAKE_LANGUAGE: &str = "fake";
const FAKE_PATH: &str = "src/lib.fake";
// The stdio client intentionally keeps listening until this deadline expires
// (late publishes are part of the LSP contract), so every successful
// collection pays the FULL timeout as wall time. Keep it small: it only has
// to cover a didOpen -> publishDiagnostics round trip against an
// already-initialized fake server.
const FAKE_LSP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(150);
// Recovery collections (re-run a healthy server after forcing a crash) must
// complete a real python spawn + didOpen -> publishDiagnostics round trip. This
// generous spawn/write bound prevents false timeouts on loaded runners; the
// recovery helper keeps the diagnostics quiet window small so success stays
// cheap.
const FAKE_LSP_RECOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
// macOS full-suite runs can briefly starve a newly spawned /usr/bin/python3
// while other test binaries are starting. Keep that harness-only startup
// allowance independent from the deliberately short diagnostics quiet window.
const FAKE_LSP_START_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const OUTER_ASYNC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);
const HANGING_WRITE_LINE_COUNT: usize = 64_000;

struct FakeLspPhaseControl {
    listener: TcpListener,
    address: std::net::SocketAddr,
}

struct ReachedFakeLspPhase {
    stream: TcpStream,
}

impl FakeLspPhaseControl {
    async fn bind() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        Self { listener, address }
    }

    fn script_preamble(&self, phases: &[&str]) -> String {
        let phases = phases
            .iter()
            .map(|phase| format!("{phase:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"
import socket

TRACEDECAY_PHASES = {{{phases}}}

def tracedecay_maybe_phase(name):
    if name not in TRACEDECAY_PHASES:
        return
    with socket.create_connection(("127.0.0.1", {port})) as control:
        control.sendall((name + "\n").encode("utf-8"))
        control.shutdown(socket.SHUT_WR)
        if control.recv(1) != b"1":
            raise RuntimeError("phase control closed before release")
"#,
            port = self.address.port(),
            phases = phases,
        )
    }

    async fn wait_for(&self, expected: &str) -> ReachedFakeLspPhase {
        let (stream, _) = self.listener.accept().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut actual = String::new();
        reader.read_line(&mut actual).await.unwrap();
        assert_eq!(actual.trim_end(), expected);
        ReachedFakeLspPhase {
            stream: reader.into_inner(),
        }
    }
}

impl ReachedFakeLspPhase {
    async fn release(mut self) {
        self.stream.write_all(b"1").await.unwrap();
    }
}

struct PendingSemanticProvider;

impl SemanticProviderPort for PendingSemanticProvider {
    fn request(
        &self,
        _root: &AdmittedRoot,
        _request_id: &LspRequestId,
        _request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        SemanticProviderOutcome::Pending
    }
}

struct UnavailableSemanticProvider;

impl SemanticProviderPort for UnavailableSemanticProvider {
    fn request(
        &self,
        _root: &AdmittedRoot,
        _request_id: &LspRequestId,
        _request: &SemanticRequest,
    ) -> SemanticProviderOutcome<SemanticResponse> {
        SemanticProviderOutcome::Unavailable
    }
}

struct FixedCancellation(bool);

impl AnalyzerCancellationPort for FixedCancellation {
    fn cancel_upstream(&self, _root: &AdmittedRoot, _request_id: &LspRequestId) -> bool {
        self.0
    }
}

fn bounded_fake_lsp_timeouts() -> lsp::client::LspRefreshTimeouts {
    lsp::client::LspRefreshTimeouts::new(
        std::time::Duration::from_millis(350),
        std::time::Duration::from_millis(350),
        std::time::Duration::from_millis(350),
        std::time::Duration::from_millis(50),
    )
}

fn recovery_fake_lsp_timeouts() -> lsp::client::LspRefreshTimeouts {
    lsp::client::LspRefreshTimeouts::new(
        FAKE_LSP_RECOVERY_TIMEOUT + FAKE_LSP_TIMEOUT,
        FAKE_LSP_RECOVERY_TIMEOUT,
        FAKE_LSP_RECOVERY_TIMEOUT,
        FAKE_LSP_TIMEOUT,
    )
}

fn loaded_runner_fake_lsp_timeouts() -> lsp::client::LspRefreshTimeouts {
    lsp::client::LspRefreshTimeouts::new(
        FAKE_LSP_TIMEOUT,
        FAKE_LSP_START_TIMEOUT,
        FAKE_LSP_START_TIMEOUT,
        FAKE_LSP_TIMEOUT,
    )
}

#[test]
fn analyzer_lifecycle_is_project_scoped_and_preserves_failure_evidence() {
    let root = AdmittedRoot::new("file:///project");
    let other = AdmittedRoot::new("file:///other");
    let mut supervisor = AnalyzerSupervisor::new(root.clone());

    assert_eq!(
        supervisor.apply(&other, AnalyzerEvent::StartRequested),
        Err(AnalyzerTransitionError::RootMismatch {
            expected: root.clone(),
            actual: other,
        })
    );
    supervisor
        .apply(&root, AnalyzerEvent::StartRequested)
        .unwrap();
    supervisor
        .apply(&root, AnalyzerEvent::TransportFailed)
        .unwrap();
    assert_eq!(supervisor.state(), AnalyzerState::RestartBackoff);
    assert_eq!(
        supervisor.failure_evidence(),
        Some(("analyzer-transport-failed", "Analyzer transport failed."))
    );

    supervisor
        .apply(&root, AnalyzerEvent::StartRequested)
        .unwrap();
    supervisor.apply(&root, AnalyzerEvent::Ready).unwrap();
    assert!(supervisor.is_ready_for(&root));
    assert_eq!(supervisor.failure_evidence(), None);
}

#[test]
fn polyglot_semantics_route_unique_extensions_and_fall_back_on_ambiguity() {
    let routed: Arc<dyn SemanticProviderPort + Send + Sync> = Arc::new(PendingSemanticProvider);
    let fallback: Arc<dyn SemanticProviderPort + Send + Sync> =
        Arc::new(UnavailableSemanticProvider);
    let root = AdmittedRoot::new("file:///project");
    let request_id = LspRequestId::Number(1);
    let request = SemanticRequest::DocumentSymbols {
        document_uri: "file:///project/src/app.TS".to_string(),
    };

    let unique = lsp::PolyglotSemanticProvider::new(
        vec![lsp::LanguageSemanticRoute::new(["ts"], Arc::clone(&routed))],
        Arc::clone(&fallback),
    );
    assert!(matches!(
        unique.request(&root, &request_id, &request),
        SemanticProviderOutcome::Pending
    ));

    let ambiguous = lsp::PolyglotSemanticProvider::new(
        vec![
            lsp::LanguageSemanticRoute::new(["ts"], Arc::clone(&routed)),
            lsp::LanguageSemanticRoute::new(["TS"], routed),
        ],
        fallback,
    );
    assert!(matches!(
        ambiguous.request(&root, &request_id, &request),
        SemanticProviderOutcome::Unavailable
    ));
}

#[test]
fn composite_analyzer_cancellation_fans_out() {
    let cancellation = lsp::CompositeAnalyzerCancellation::new(vec![
        Arc::new(FixedCancellation(false)),
        Arc::new(FixedCancellation(true)),
    ]);

    assert!(cancellation.cancel_upstream(
        &AdmittedRoot::new("file:///project"),
        &LspRequestId::Number(2)
    ));
}

#[test]
fn broker_exposes_project_scoped_readiness_without_lsp_transport() {
    let project = tempfile::tempdir().unwrap();
    let script_path = project.path().join("fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        project.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let root_uri = url::Url::from_directory_path(project.path())
        .unwrap()
        .to_string();

    let authority = broker
        .semantic_authority_if_available(
            FAKE_LANGUAGE,
            project.path().to_path_buf(),
            root_uri.clone(),
            bounded_fake_lsp_timeouts(),
        )
        .unwrap()
        .expect("fake analyzer is executable");
    let readiness = authority.analyzer_readiness();

    assert_eq!(readiness.root().uri(), root_uri);
    assert_eq!(readiness.state(), AnalyzerState::AwaitingStart);
    assert_eq!(readiness.failure_evidence(), None);
}

#[test]
fn broker_rejects_analyzer_workspace_outside_project_scope() {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let script_path = project.path().join("fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        project.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let root_uri = url::Url::from_directory_path(project.path())
        .unwrap()
        .to_string();

    let result = broker.semantic_authority_if_available(
        FAKE_LANGUAGE,
        outside.path().to_path_buf(),
        root_uri,
        bounded_fake_lsp_timeouts(),
    );

    assert!(matches!(
        result,
        Err(error) if error.to_string().contains("outside the admitted project root")
    ));
}

#[test]
fn broker_rejects_analyzer_root_uri_for_another_project() {
    let project = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let script_path = project.path().join("fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        project.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let other_uri = url::Url::from_directory_path(other.path())
        .unwrap()
        .to_string();

    let result = broker.semantic_authority_if_available(
        FAKE_LANGUAGE,
        project.path().to_path_buf(),
        other_uri,
        bounded_fake_lsp_timeouts(),
    );

    assert!(matches!(
        result,
        Err(error) if error.to_string().contains("does not match the admitted project root")
    ));
}

#[test]
fn settings_disable_language_and_backfill_mode_round_trip() {
    let mut settings = lsp::settings::CodeDiagnosticsSettings::default();
    settings.set_language_enabled("rust", false);
    settings
        .languages
        .entry("rust".to_string())
        .or_default()
        .command_override = Some("/opt/bin/rust-analyzer".to_string());
    settings.idle_backfill = lsp::settings::IdleBackfillMode::Off;
    settings
        .custom_adapters
        .push(lsp::adapters::LspAdapterDefinition {
            language: "ruby".to_string(),
            language_id: "ruby".to_string(),
            command: "ruby-lsp".to_string(),
            args: Vec::new(),
            extensions: vec!["rb".to_string()],
            root_markers: vec!["Gemfile".to_string()],
            install_options: Vec::new(),
            diagnostics: lsp::adapters::DiagnosticMode::Push,
        });

    let encoded = serde_json::to_string(&settings).unwrap();
    let decoded: lsp::settings::CodeDiagnosticsSettings = serde_json::from_str(&encoded).unwrap();

    assert!(!decoded.language_enabled("rust"));
    assert_eq!(decoded.idle_backfill, lsp::settings::IdleBackfillMode::Off);
    assert_eq!(
        decoded.command_for("rust", "rust-analyzer"),
        "/opt/bin/rust-analyzer"
    );
    assert_eq!(decoded.custom_adapters[0].language, "ruby");
}

#[tokio::test]
async fn settings_persist_under_dashboard_root() {
    let temp = tempfile::tempdir().unwrap();
    let mut settings = lsp::settings::CodeDiagnosticsSettings::default();
    settings.set_language_enabled("python", false);
    settings.idle_backfill = lsp::settings::IdleBackfillMode::Off;

    lsp::settings::save_settings(temp.path(), &settings)
        .await
        .unwrap();
    let loaded = lsp::settings::load_settings(temp.path()).await.unwrap();

    assert!(!loaded.language_enabled("python"));
    assert_eq!(loaded.idle_backfill, lsp::settings::IdleBackfillMode::Off);

    let replacement = lsp::settings::CodeDiagnosticsSettings::default();
    lsp::settings::save_settings(temp.path(), &replacement)
        .await
        .unwrap();
    assert_eq!(
        lsp::settings::load_settings(temp.path()).await.unwrap(),
        replacement
    );
    let backup = lsp::settings::settings_path(temp.path()).with_extension("json.bak");
    let backup: lsp::settings::CodeDiagnosticsSettings =
        serde_json::from_slice(&tokio::fs::read(backup).await.unwrap()).unwrap();
    assert_eq!(backup, settings);
}

#[tokio::test]
async fn stdio_client_collects_publish_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake_lsp.py");
    let control = FakeLspPhaseControl::bind().await;
    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble(
            &control.script_preamble(&["initialized", "document-written"]),
            FAKE_DIAGNOSTIC_PUBLISH,
        ),
    )
    .unwrap();

    let root = temp.path().to_path_buf();
    let collect = tokio::spawn(async move {
        lsp::client::collect_document_diagnostics(
            python_command(),
            &[script_path.display().to_string()],
            &root,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            FAKE_LSP_TIMEOUT,
        )
        .await
    });
    control.wait_for("initialized").await.release().await;
    control.wait_for("document-written").await.release().await;
    let diagnostics = collect.await.unwrap().unwrap();

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].file, "src/lib.fake");
    assert_eq!(diagnostics[0].line_start, 1);
    assert_eq!(
        diagnostics[0].severity,
        lsp::broker::DiagnosticSeverity::Error
    );
    assert_eq!(diagnostics[0].code.as_deref(), Some("E_FAKE"));
}

#[tokio::test]
async fn stdio_client_keeps_listening_after_initial_empty_publish() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("late_fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script_with_initial_empty_publish()).unwrap();

    let diagnostics = lsp::client::collect_document_diagnostics(
        python_command(),
        &[script_path.display().to_string()],
        temp.path(),
        vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
        // The fake server emits the empty and the late publish back to back
        // (no wall-clock delay), so the deadline only has to cover one
        // didOpen -> publishDiagnostics round trip, same as the other fake
        // LSP tests. The client processes the messages in order either way,
        // which is the behaviour under test.
        FAKE_LSP_TIMEOUT,
    )
    .await
    .unwrap();

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message, "late semantic error");
}

#[tokio::test]
async fn broker_refresh_documents_populates_cached_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );

    broker
        .refresh_documents(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            FAKE_LSP_TIMEOUT,
        )
        .await
        .unwrap();

    let snapshot = broker.snapshot();
    assert_eq!(snapshot.summary.total_errors, 1);
    assert_eq!(snapshot.diagnostics[0].source, "fake-ls");
    assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Ready);
}

#[test]
fn broker_marks_active_command_available_before_first_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    let broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );

    let snapshot = broker.snapshot();
    assert_engine_state(
        &snapshot,
        FAKE_LANGUAGE,
        lsp::broker::EngineState::Available,
    );
}

#[tokio::test]
async fn broker_keeps_diagnostics_for_multiple_languages_in_one_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![
            fake_python_adapter("alpha", "alpha", &script_path),
            fake_python_adapter("beta", "beta", &script_path),
        ],
    );

    broker
        .refresh_documents_with_timeouts(
            "alpha",
            vec![fake_document("alpha", "src/lib.alpha", "alpha nope")],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .unwrap();
    broker
        .refresh_documents_with_timeouts(
            "beta",
            vec![fake_document("beta", "src/lib.beta", "beta nope")],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .unwrap();

    let snapshot = broker.snapshot();
    assert_eq!(snapshot.summary.total_errors, 2);
    assert!(
        snapshot
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.language == "alpha" && diagnostic.file == "src/lib.alpha")
    );
    assert!(
        snapshot
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.language == "beta" && diagnostic.file == "src/lib.beta")
    );
    for language in ["alpha", "beta"] {
        assert_engine_state(&snapshot, language, lsp::broker::EngineState::Ready);
    }
}

#[tokio::test]
async fn broker_marks_missing_lsp_command_unavailable_after_refresh_failure() {
    let temp = tempfile::tempdir().unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_adapter(
            FAKE_LANGUAGE,
            "fake",
            "__tracedecay_missing_lsp_for_test__",
            Vec::new(),
        )],
    );

    let err = broker
        .refresh_documents(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            std::time::Duration::from_millis(50),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("not available on PATH"));
    let snapshot = broker.snapshot();
    let status = engine_status(&snapshot, FAKE_LANGUAGE);
    assert_eq!(status.state, lsp::broker::EngineState::Unavailable);
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("not available on PATH")
    );
}

#[tokio::test]
async fn broker_marks_initialize_exit_crashed_without_message_classification() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("missing_component_lsp.py");
    let control = FakeLspPhaseControl::bind().await;
    std::fs::write(
        &script_path,
        format!(
            r#"
import sys

{control}
sys.stderr.write("error: unknown binary 'rust-analyzer' in toolchain 'test-toolchain'\n")
sys.stderr.flush()
tracedecay_maybe_phase("process-exit")
"#,
            control = control.script_preamble(&["process-exit"]),
        ),
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );

    let refresh = tokio::spawn(async move {
        let result = broker
            .refresh_documents(
                FAKE_LANGUAGE,
                vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
                std::time::Duration::from_millis(500),
            )
            .await;
        (broker, result)
    });
    control.wait_for("process-exit").await.release().await;
    let (broker, result) = refresh.await.unwrap();
    let err = result.unwrap_err();

    assert!(err.to_string().contains("unknown binary"));
    let snapshot = broker.snapshot();
    let status = engine_status(&snapshot, FAKE_LANGUAGE);
    assert_eq!(status.state, lsp::broker::EngineState::Crashed);
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("unknown binary")
    );
}

#[tokio::test]
async fn broker_bounds_lsp_initialize_hangs() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("hanging_initialize_lsp.py");
    std::fs::write(&script_path, fake_lsp_script_that_never_initializes()).unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );

    let result = tokio::time::timeout(
        OUTER_ASYNC_TIMEOUT,
        broker.refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            bounded_fake_lsp_timeouts(),
        ),
    )
    .await;

    let err = result
        .expect("refresh should be bounded by the diagnostics timeout")
        .expect_err("hung initialize should crash the engine");
    assert!(err.to_string().contains("timed out"));
    let snapshot = broker.snapshot();
    let status = engine_status(&snapshot, FAKE_LANGUAGE);
    assert_eq!(status.state, lsp::broker::EngineState::Crashed);
    assert!(
        status
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("timed out")
    );
}

#[tokio::test]
async fn broker_bounds_lsp_document_write_hangs() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("hanging_write_lsp.py");
    std::fs::write(
        &script_path,
        fake_lsp_script_that_initializes_then_stops_reading(),
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );

    let result = tokio::time::timeout(
        OUTER_ASYNC_TIMEOUT,
        broker.refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(
                FAKE_LANGUAGE,
                FAKE_PATH,
                &"let nope\n".repeat(HANGING_WRITE_LINE_COUNT),
            )],
            bounded_fake_lsp_timeouts(),
        ),
    )
    .await;

    let err = result
        .expect("refresh should be bounded while writing document text")
        .expect_err("hung document write should crash the engine");
    assert!(err.to_string().contains("timed out"));
    let snapshot = broker.snapshot();
    assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Crashed);

    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap();

    let snapshot = broker.snapshot();
    assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Ready);
    assert_eq!(snapshot.summary.total_errors, 1);
}

#[tokio::test]
async fn stdio_client_bounds_lsp_document_write_hangs() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("hanging_write_lsp.py");
    std::fs::write(
        &script_path,
        fake_lsp_script_that_initializes_then_stops_reading(),
    )
    .unwrap();

    let result = tokio::time::timeout(
        OUTER_ASYNC_TIMEOUT,
        lsp::client::collect_document_diagnostics_with_timeouts(
            python_command(),
            &[script_path.display().to_string()],
            temp.path(),
            vec![fake_document(
                FAKE_LANGUAGE,
                FAKE_PATH,
                &"let nope\n".repeat(HANGING_WRITE_LINE_COUNT),
            )],
            bounded_fake_lsp_timeouts(),
        ),
    )
    .await;

    let err = result
        .expect("client collection should be bounded while writing document text")
        .expect_err("hung document write should fail the client collection");
    assert!(err.to_string().contains("timed out"));
}

#[tokio::test]
async fn stdio_client_allows_slow_but_progressing_document_writes() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("slow_reader_lsp.py");
    std::fs::write(
        &script_path,
        fake_lsp_script_that_reads_large_messages_slowly(),
    )
    .unwrap();

    let diagnostics = tokio::time::timeout(
        OUTER_ASYNC_TIMEOUT,
        lsp::client::collect_document_diagnostics(
            python_command(),
            &[script_path.display().to_string()],
            temp.path(),
            vec![fake_document(
                FAKE_LANGUAGE,
                FAKE_PATH,
                &"let nope\n".repeat(80_000),
            )],
            std::time::Duration::from_millis(500),
        ),
    )
    .await
    .expect("slow progressing write should stay bounded")
    .expect("slow progressing write should not crash the client");

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].message, "fake type error");
}

#[tokio::test]
async fn broker_drops_lsp_client_after_partial_diagnostics_frame_timeout() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("partial_frame_lsp.py");
    std::fs::write(&script_path, fake_lsp_script_with_partial_publish()).unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );

    let err = broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            bounded_fake_lsp_timeouts(),
        )
        .await
        .expect_err("partial diagnostics frame should crash the cached client");
    assert!(err.to_string().contains("timed out"));
    let snapshot = broker.snapshot();
    assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Crashed);

    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap();

    let snapshot = broker.snapshot();
    assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Ready);
    assert_eq!(snapshot.summary.total_errors, 1);
}

#[tokio::test]
async fn stdio_client_fails_when_no_document_publishes_diagnostics() {
    // Preserve the #237 behavior: a genuine timeout where NOTHING arrived is
    // still an error and must not be recorded as a complete (empty) refresh.
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("silent_lsp.py");
    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble("", NEVER_PUBLISH),
    )
    .unwrap();

    let err = lsp::client::collect_document_diagnostics(
        python_command(),
        &[script_path.display().to_string()],
        temp.path(),
        vec![
            fake_document(FAKE_LANGUAGE, "src/first.fake", "let nope"),
            fake_document(FAKE_LANGUAGE, "src/second.fake", "let clean"),
        ],
        std::time::Duration::from_millis(50),
    )
    .await
    .expect_err("a batch with zero publishes should fail as a timeout");

    assert!(
        err.to_string().contains("timed out"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn stdio_client_returns_diagnostics_for_suppress_empty_servers() {
    // A server that only publishes for files WITH problems (never an empty
    // publish for clean files) produces a "partial" batch: `first.fake` reports
    // an error, `second.fake` (clean) never publishes. The refresh must return
    // the real diagnostics for the file that responded rather than dropping the
    // whole batch as a timeout.
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("one_document_lsp.py");
    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble("", FIRST_URI_ONLY_DIAGNOSTIC_PUBLISH),
    )
    .unwrap();

    let diagnostics = lsp::client::collect_document_diagnostics(
        python_command(),
        &[script_path.display().to_string()],
        temp.path(),
        vec![
            fake_document(FAKE_LANGUAGE, "src/first.fake", "let nope"),
            fake_document(FAKE_LANGUAGE, "src/second.fake", "let clean"),
        ],
        std::time::Duration::from_millis(50),
    )
    .await
    .expect("diagnostics from the responding file should be returned, not dropped");

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].file, "src/first.fake");
    assert_eq!(diagnostics[0].message, "first file error");
}

#[tokio::test]
async fn broker_cancels_partial_refresh_without_poisoning_warm_client() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("cancel_partial_lsp.py");
    let control = FakeLspPhaseControl::bind().await;
    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble(
            &control.script_preamble(&["partial-frame-flushed"]),
            PARTIAL_DIAGNOSTIC_PUBLISH,
        ),
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let prepared = broker
        .prepare_refresh(
            FAKE_LANGUAGE,
            vec![fake_document(
                FAKE_LANGUAGE,
                "src/canceled.fake",
                "let nope",
            )],
        )
        .unwrap()
        .expect("refresh should prepare");
    let handle = tokio::spawn(async move {
        prepared
            .collect_diagnostics(std::time::Duration::from_millis(500))
            .await
    });
    control
        .wait_for("partial-frame-flushed")
        .await
        .release()
        .await;
    handle.abort();
    let _ = handle.await;

    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    // The property under test is that aborting a partial refresh does not
    // poison the broker: a *subsequent* refresh must spin up a clean client.
    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .expect("next refresh should start a clean client and recover");

    let snapshot = broker.snapshot();
    assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Ready);
    assert_eq!(snapshot.summary.total_errors, 1);
}

#[tokio::test]
async fn broker_waits_for_active_lsp_refresh_instead_of_timing_out_lock() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake_lsp.py");
    let counter_path = temp.path().join("starts.txt");
    let control = FakeLspPhaseControl::bind().await;
    let preamble = format!(
        r#"
with open({counter_path:?}, "a", encoding="utf-8") as f:
    f.write("start\n")
{control}
"#,
        counter_path = counter_path.display().to_string(),
        control = control.script_preamble(&["write-acquired"]),
    );
    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble(&preamble, EMPTY_DIAGNOSTIC_PUBLISH),
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let first = broker
        .prepare_refresh(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
        )
        .unwrap()
        .expect("first refresh should prepare");
    let second = broker
        .prepare_refresh(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
        )
        .unwrap()
        .expect("second refresh should prepare");

    let first_refresh = tokio::spawn(async move {
        first
            .collect_diagnostics(std::time::Duration::from_millis(500))
            .await
    });
    let write_acquired = control.wait_for("write-acquired").await;
    let (second_started_tx, second_started_rx) = tokio::sync::oneshot::channel();
    let second_refresh = tokio::spawn(async move {
        let _ = second_started_tx.send(());
        second
            .collect_diagnostics(std::time::Duration::from_millis(50))
            .await
    });
    second_started_rx.await.unwrap();
    write_acquired.release().await;
    let first_completed = first_refresh.await.unwrap();
    let second_completed = second_refresh.await.unwrap();

    assert!(first_completed.is_ok());
    assert!(
        second_completed.is_ok(),
        "short second refresh should wait for the active refresh instead of timing out the client lock"
    );
    let starts = std::fs::read_to_string(counter_path).unwrap();
    assert_eq!(starts.lines().count(), 1);
}

#[tokio::test]
async fn broker_ignores_stale_refresh_completion() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let stale = broker
        .prepare_refresh(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, "src/stale.fake", "let nope")],
        )
        .unwrap()
        .expect("stale refresh should prepare");
    let latest = broker
        .prepare_refresh(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, "src/latest.fake", "let nope")],
        )
        .unwrap()
        .expect("latest refresh should prepare");

    let latest_completed = latest
        .collect_diagnostics_with_timeouts(loaded_runner_fake_lsp_timeouts())
        .await;
    assert!(latest_completed.is_ok());
    broker.finish_refresh(latest_completed).unwrap();

    let stale_completed = stale
        .collect_diagnostics_with_timeouts(loaded_runner_fake_lsp_timeouts())
        .await;
    assert!(stale_completed.is_ok());
    broker.finish_refresh(stale_completed).unwrap();

    let snapshot = broker.snapshot();
    assert!(
        snapshot
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.file == "src/latest.fake")
    );
    assert!(
        !snapshot
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.file == "src/stale.fake")
    );
}

#[tokio::test]
async fn broker_reuses_warm_lsp_client_between_refreshes() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("warm_fake_lsp.py");
    let counter_path = temp.path().join("starts.txt");
    std::fs::write(
        &script_path,
        fake_lsp_script_that_records_start(&counter_path),
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let document = fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope");

    broker
        .refresh_documents("fake", vec![document.clone()], FAKE_LSP_TIMEOUT)
        .await
        .unwrap();
    broker
        .refresh_documents("fake", vec![document], FAKE_LSP_TIMEOUT)
        .await
        .unwrap();

    let starts = std::fs::read_to_string(counter_path).unwrap();
    assert_eq!(starts.lines().count(), 1);
}

#[tokio::test]
async fn broker_keys_warm_lsp_clients_by_workspace_root() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("workspace_fake_lsp.py");
    let counter_path = temp.path().join("starts.txt");
    std::fs::write(
        &script_path,
        fake_lsp_script_that_records_start(&counter_path),
    )
    .unwrap();
    std::fs::create_dir_all(temp.path().join("workspace-a/src")).unwrap();
    std::fs::create_dir_all(temp.path().join("workspace-b/src")).unwrap();
    std::fs::write(temp.path().join("workspace-a/fake-root"), "").unwrap();
    std::fs::write(temp.path().join("workspace-b/fake-root"), "").unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_adapter_with_root_marker(
            FAKE_LANGUAGE,
            "fake",
            python_command(),
            vec![script_path.display().to_string()],
            "fake-root",
        )],
    );
    let documents = vec![
        fake_document(FAKE_LANGUAGE, "workspace-a/src/lib.fake", "let nope"),
        fake_document(FAKE_LANGUAGE, "workspace-b/src/lib.fake", "let nope"),
    ];

    broker
        .refresh_documents_with_timeouts(
            "fake",
            documents.clone(),
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .unwrap();
    broker
        .refresh_documents_with_timeouts("fake", documents, loaded_runner_fake_lsp_timeouts())
        .await
        .unwrap();

    let starts = std::fs::read_to_string(counter_path).unwrap();
    assert_eq!(starts.lines().count(), 2);
}

#[tokio::test]
async fn broker_ignores_refresh_completion_after_language_is_disabled() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let prepared = broker
        .prepare_refresh(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
        )
        .unwrap()
        .expect("enabled language should prepare a refresh");

    broker.set_language_enabled(FAKE_LANGUAGE, false);
    let completed = prepared.collect_diagnostics(FAKE_LSP_TIMEOUT).await;
    broker.finish_refresh(completed).unwrap();

    let snapshot = broker.snapshot();
    assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Disabled);
    assert!(snapshot.diagnostics.is_empty());
}

#[test]
fn broker_clears_language_diagnostics_when_disabled() {
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        "/tmp/tracedecay-lsp-test",
        vec![fake_adapter(
            "typescript",
            "ts",
            "typescript-language-server",
            Vec::new(),
        )],
    );
    broker.cache_diagnostic(lsp::broker::CodeDiagnostic {
        language: "typescript".to_string(),
        source: "typescript-language-server".to_string(),
        file: "src/app.ts".to_string(),
        line_start: 3,
        line_end: 3,
        character_start: Some(10),
        character_end: Some(12),
        severity: lsp::broker::DiagnosticSeverity::Error,
        code: Some("TS2322".to_string()),
        message: "Type 'string' is not assignable to type 'number'.".to_string(),
        enclosing_node: None,
        updated_at: 42,
    });
    broker.record_backfill_progress("typescript", 8, 3, 1, Some(99));

    broker.set_language_enabled("typescript", false);

    let snapshot = broker.snapshot();
    assert!(snapshot.diagnostics.is_empty());
    assert!(!snapshot.backfill.contains_key("typescript"));
    assert_eq!(snapshot.summary.total_errors, 0);
}

#[tokio::test]
async fn broker_resolve_enclosing_nodes_attributes_diagnostic_to_smallest_span() {
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        "/tmp/tracedecay-lsp-test",
        vec![fake_adapter("rust", "rs", "rust-analyzer", Vec::new())],
    );
    // Diagnostic inside a known function (1-based line 5 -> 0-based line 4).
    broker.cache_diagnostic(lsp::broker::CodeDiagnostic {
        language: "rust".to_string(),
        source: "rust-analyzer".to_string(),
        file: "src/lib.rs".to_string(),
        line_start: 5,
        line_end: 5,
        character_start: Some(4),
        character_end: Some(8),
        severity: lsp::broker::DiagnosticSeverity::Error,
        code: Some("E0308".to_string()),
        message: "mismatched types".to_string(),
        enclosing_node: None,
        updated_at: 1,
    });
    // Diagnostic on a line no indexed span covers stays unattributed.
    broker.cache_diagnostic(lsp::broker::CodeDiagnostic {
        language: "rust".to_string(),
        source: "rust-analyzer".to_string(),
        file: "src/lib.rs".to_string(),
        line_start: 40,
        line_end: 40,
        character_start: None,
        character_end: None,
        severity: lsp::broker::DiagnosticSeverity::Warning,
        code: None,
        message: "unused variable".to_string(),
        enclosing_node: None,
        updated_at: 1,
    });

    broker
        .resolve_enclosing_nodes(|file| async move {
            assert_eq!(file, "src/lib.rs");
            vec![
                // Outer impl block spanning the whole region.
                lsp::broker::NodeSpan {
                    start_line: 0,
                    end_line: 20,
                    qualified_name: "crate::Widget".to_string(),
                },
                // Inner method: the smallest span covering line 4.
                lsp::broker::NodeSpan {
                    start_line: 3,
                    end_line: 9,
                    qualified_name: "crate::Widget::render".to_string(),
                },
            ]
        })
        .await;

    let snapshot = broker.snapshot();
    let attributed = snapshot
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.line_start == 5)
        .expect("diagnostic at line 5 should be present");
    assert_eq!(
        attributed.enclosing_node.as_deref(),
        Some("crate::Widget::render"),
        "diagnostic should attribute to the smallest enclosing indexed node"
    );
    let unattributed = snapshot
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.line_start == 40)
        .expect("diagnostic at line 40 should be present");
    assert_eq!(
        unattributed.enclosing_node, None,
        "diagnostic outside every indexed span stays unattributed"
    );
}

fn assert_engine_state(
    snapshot: &lsp::broker::DiagnosticsSnapshot,
    language: &str,
    state: lsp::broker::EngineState,
) {
    assert_eq!(engine_status(snapshot, language).state, state);
}

fn engine_status<'a>(
    snapshot: &'a lsp::broker::DiagnosticsSnapshot,
    language: &str,
) -> &'a lsp::broker::EngineStatus {
    snapshot
        .engines
        .iter()
        .find(|engine| engine.language == language)
        .unwrap_or_else(|| panic!("{language} engine status should be listed"))
}

fn fake_document(language: &str, relative_path: &str, text: &str) -> lsp::client::LspDocument {
    lsp::client::LspDocument {
        language: language.to_string(),
        language_id: language.to_string(),
        relative_path: relative_path.to_string(),
        text: text.to_string(),
    }
}

fn fake_python_adapter(
    language: &str,
    extension: &str,
    script_path: &std::path::Path,
) -> lsp::adapters::LspAdapterDefinition {
    fake_adapter(
        language,
        extension,
        python_command(),
        vec![script_path.display().to_string()],
    )
}

fn python_command() -> &'static str {
    if cfg!(windows) {
        "python"
    } else if std::path::Path::new("/usr/bin/python3").is_file() {
        "/usr/bin/python3"
    } else {
        "python3"
    }
}

fn fake_adapter(
    language: &str,
    extension: &str,
    command: &str,
    args: Vec<String>,
) -> lsp::adapters::LspAdapterDefinition {
    fake_adapter_with_root_markers(language, extension, command, args, Vec::new())
}

fn fake_adapter_with_root_marker(
    language: &str,
    extension: &str,
    command: &str,
    args: Vec<String>,
    root_marker: &str,
) -> lsp::adapters::LspAdapterDefinition {
    fake_adapter_with_root_markers(
        language,
        extension,
        command,
        args,
        vec![root_marker.to_string()],
    )
}

fn fake_adapter_with_root_markers(
    language: &str,
    extension: &str,
    command: &str,
    args: Vec<String>,
    root_markers: Vec<String>,
) -> lsp::adapters::LspAdapterDefinition {
    lsp::adapters::LspAdapterDefinition {
        language: language.to_string(),
        language_id: language.to_string(),
        command: command.to_string(),
        args,
        extensions: vec![extension.to_string()],
        root_markers,
        install_options: Vec::new(),
        diagnostics: lsp::adapters::DiagnosticMode::Push,
    }
}

fn fake_lsp_script() -> String {
    fake_lsp_script_with_preamble("", FAKE_DIAGNOSTIC_PUBLISH)
}

fn fake_lsp_script_with_initial_empty_publish() -> String {
    fake_lsp_script_with_preamble("", INITIAL_EMPTY_THEN_DIAGNOSTIC_PUBLISH)
}

fn fake_lsp_script_that_records_start(counter_path: &std::path::Path) -> String {
    let preamble = format!(
        r#"
with open({:?}, "a", encoding="utf-8") as f:
    f.write("start\n")
"#,
        counter_path.display().to_string()
    );
    fake_lsp_script_with_preamble(&preamble, EMPTY_DIAGNOSTIC_PUBLISH)
}

fn fake_lsp_script_that_never_initializes() -> &'static str {
    r#"
import time

time.sleep(60)
"#
}

fn fake_lsp_script_that_initializes_then_stops_reading() -> &'static str {
    r#"
import json
import sys
import time

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        headers[name.lower()] = value.strip()
    length = int(headers["content-length"])
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))

def send(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n" + body)
    sys.stdout.buffer.flush()

message = read_message()
send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {"textDocumentSync": 1}}})
time.sleep(60)
"#
}

fn fake_lsp_script_that_reads_large_messages_slowly() -> &'static str {
    r#"
import json
import sys
import time

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        headers[name.lower()] = value.strip()
    length = int(headers["content-length"])
    remaining = length
    chunks = []
    while remaining:
        chunk = sys.stdin.buffer.read(min(4096, remaining))
        if not chunk:
            return None
        chunks.append(chunk)
        remaining -= len(chunk)
        time.sleep(0.00075)
    return json.loads(b"".join(chunks).decode("utf-8"))

def send(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n" + body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    if message.get("method") == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {"textDocumentSync": 1}}})
    elif message.get("method") == "textDocument/didChange":
        uri = message["params"]["textDocument"]["uri"]
        send({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [{
                    "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 9}},
                    "severity": 1,
                    "source": "fake-ls",
                    "message": "fake type error"
                }]
            }
        })
"#
}

fn fake_lsp_script_with_partial_publish() -> String {
    fake_lsp_script_with_preamble("", PARTIAL_DIAGNOSTIC_PUBLISH)
}

fn fake_lsp_script_with_preamble(preamble: &str, did_open_body: &str) -> String {
    let mut script = String::from(
        r#"
import json
import sys

"#,
    );
    script.push_str(preamble);
    script.push_str(
        r#"
def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        headers[name.lower()] = value.strip()
    length = int(headers["content-length"])
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))

def send(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n" + body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    if message.get("method") == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {"textDocumentSync": 1}}})
    elif message.get("method") == "initialized":
        if "tracedecay_maybe_phase" in globals():
            tracedecay_maybe_phase("initialized")
    elif message.get("method") == "textDocument/didOpen":
        uri = message["params"]["textDocument"]["uri"]
        if "tracedecay_maybe_phase" in globals():
            tracedecay_maybe_phase("document-written")
            tracedecay_maybe_phase("write-acquired")
"#,
    );
    script.push_str(did_open_body);
    script.push_str(
        r#"    elif message.get("method") == "textDocument/didChange":
        uri = message["params"]["textDocument"]["uri"]
"#,
    );
    script.push_str(did_open_body);
    script
}

const FAKE_DIAGNOSTIC_PUBLISH: &str = r#"        send({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [{
                    "range": {
                        "start": {"line": 0, "character": 4},
                        "end": {"line": 0, "character": 9}
                    },
                    "severity": 1,
                    "code": "E_FAKE",
                    "source": "fake-ls",
                    "message": "fake type error"
                }]
            }
        })
"#;

const INITIAL_EMPTY_THEN_DIAGNOSTIC_PUBLISH: &str = r#"        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": []}})
        send({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": [{
                    "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
                    "severity": 1,
                    "source": "fake-ls",
                    "message": "late semantic error"
                }]
            }
        })
"#;

const PARTIAL_DIAGNOSTIC_PUBLISH: &str = r#"        sys.stdout.buffer.write(b"Content-Length: 120\r\n\r\n{\"jsonrpc\":\"2.0\"")
        sys.stdout.buffer.flush()
        if "tracedecay_maybe_phase" in globals():
            tracedecay_maybe_phase("partial-frame-flushed")
        import time
        time.sleep(60)
"#;

const NEVER_PUBLISH: &str = r#"        _ = uri
        pass
"#;

const FIRST_URI_ONLY_DIAGNOSTIC_PUBLISH: &str = r#"        if uri.endswith("/first.fake"):
            send({
                "jsonrpc": "2.0",
                "method": "textDocument/publishDiagnostics",
                "params": {
                    "uri": uri,
                    "diagnostics": [{
                        "range": {"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 9}},
                        "severity": 1,
                        "source": "fake-ls",
                        "message": "first file error"
                    }]
                }
            })
"#;

const EMPTY_DIAGNOSTIC_PUBLISH: &str = r#"        send({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": []
            }
        })
"#;
