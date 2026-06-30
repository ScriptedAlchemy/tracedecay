#![allow(clippy::expect_used, clippy::unwrap_used)]

use tracedecay::diagnostics::lsp;

const FAKE_LANGUAGE: &str = "fake";
const FAKE_PATH: &str = "src/lib.fake";

#[test]
fn builtin_registry_advertises_phase_one_setup_contract() {
    let adapters = lsp::adapters::builtin_adapters();
    for language in [
        "rust",
        "typescript",
        "javascript",
        "python",
        "go",
        "c",
        "cpp",
        "objc",
        "zig",
        "lua",
        "php",
    ] {
        let adapter = adapter(&adapters, language);
        assert!(
            !adapter.extensions.is_empty(),
            "{language} should advertise file extensions"
        );
        assert!(
            !adapter.install_options.is_empty(),
            "{language} should expose setup help"
        );
    }

    assert_eq!(adapter(&adapters, "typescript").args, ["--stdio"]);
    assert_eq!(adapter(&adapters, "javascript").args, ["--stdio"]);
    assert_eq!(adapter(&adapters, "python").args, ["--stdio"]);
    assert!(adapter(&adapters, "typescript").install_options[0]
        .command
        .contains("typescript-language-server"));
    assert!(adapter(&adapters, "rust").install_options[0]
        .command
        .contains("rust-analyzer"));
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
}

#[tokio::test]
async fn stdio_client_collects_publish_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script()).unwrap();

    let diagnostics = lsp::client::collect_document_diagnostics(
        python_command(),
        &[script_path.display().to_string()],
        temp.path(),
        vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
        std::time::Duration::from_secs(3),
    )
    .await
    .unwrap();

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
        std::time::Duration::from_millis(500),
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
            std::time::Duration::from_secs(3),
        )
        .await
        .unwrap();

    let snapshot = broker.snapshot();
    assert_eq!(snapshot.summary.total_errors, 1);
    assert_eq!(snapshot.diagnostics[0].source, "fake-ls");
    assert_eq!(
        snapshot
            .engines
            .iter()
            .find(|engine| engine.language == "fake")
            .unwrap()
            .state,
        lsp::broker::EngineState::Ready
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
        .refresh_documents(
            "alpha",
            vec![fake_document("alpha", "src/lib.alpha", "alpha nope")],
            std::time::Duration::from_secs(3),
        )
        .await
        .unwrap();
    broker
        .refresh_documents(
            "beta",
            vec![fake_document("beta", "src/lib.beta", "beta nope")],
            std::time::Duration::from_secs(3),
        )
        .await
        .unwrap();

    let snapshot = broker.snapshot();
    assert_eq!(snapshot.summary.total_errors, 2);
    assert!(snapshot
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.language == "alpha" && diagnostic.file == "src/lib.alpha"));
    assert!(snapshot
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.language == "beta" && diagnostic.file == "src/lib.beta"));
    for language in ["alpha", "beta"] {
        let status = snapshot
            .engines
            .iter()
            .find(|engine| engine.language == language)
            .unwrap_or_else(|| panic!("missing {language} status"));
        assert_eq!(status.state, lsp::broker::EngineState::Ready);
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
    let status = snapshot
        .engines
        .iter()
        .find(|engine| engine.language == "fake")
        .expect("fake engine status should be listed");
    assert_eq!(status.state, lsp::broker::EngineState::Unavailable);
    assert!(status
        .last_error
        .as_deref()
        .unwrap_or_default()
        .contains("not available on PATH"));
}

#[tokio::test]
async fn broker_marks_install_proxy_exit_during_initialize_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("missing_component_lsp.py");
    std::fs::write(
        &script_path,
        r#"
import sys

sys.stderr.write("error: unknown binary 'rust-analyzer' in toolchain 'test-toolchain'\n")
sys.stderr.flush()
"#,
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );

    let err = broker
        .refresh_documents(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            std::time::Duration::from_millis(50),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("unknown binary"));
    let snapshot = broker.snapshot();
    let status = snapshot
        .engines
        .iter()
        .find(|engine| engine.language == FAKE_LANGUAGE)
        .expect("fake engine status should be listed");
    assert_eq!(status.state, lsp::broker::EngineState::Unavailable);
    assert!(status
        .last_error
        .as_deref()
        .unwrap_or_default()
        .contains("unknown binary"));
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
        .refresh_documents(
            "fake",
            vec![document.clone()],
            std::time::Duration::from_secs(3),
        )
        .await
        .unwrap();
    broker
        .refresh_documents("fake", vec![document], std::time::Duration::from_secs(3))
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
        .refresh_documents("fake", documents.clone(), std::time::Duration::from_secs(3))
        .await
        .unwrap();
    broker
        .refresh_documents("fake", documents, std::time::Duration::from_secs(3))
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
    let completed = prepared
        .collect_diagnostics(std::time::Duration::from_secs(3))
        .await;
    broker.finish_refresh(completed).unwrap();

    let snapshot = broker.snapshot();
    let status = snapshot
        .engines
        .iter()
        .find(|engine| engine.language == FAKE_LANGUAGE)
        .expect("fake engine status should be listed");
    assert_eq!(status.state, lsp::broker::EngineState::Disabled);
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

fn adapter<'a>(
    adapters: &'a [lsp::adapters::LspAdapterDefinition],
    language: &str,
) -> &'a lsp::adapters::LspAdapterDefinition {
    adapters
        .iter()
        .find(|adapter| adapter.language == language)
        .unwrap_or_else(|| panic!("missing adapter for {language}"))
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
    fake_lsp_script_with_preamble("import time\n", INITIAL_EMPTY_THEN_DIAGNOSTIC_PUBLISH)
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
    elif message.get("method") == "textDocument/didOpen":
        uri = message["params"]["textDocument"]["uri"]
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
        time.sleep(0.05)
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

const EMPTY_DIAGNOSTIC_PUBLISH: &str = r#"        send({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri,
                "diagnostics": []
            }
        })
"#;
