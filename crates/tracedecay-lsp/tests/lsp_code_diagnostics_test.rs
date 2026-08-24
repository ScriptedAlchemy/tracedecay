#![allow(clippy::expect_used, clippy::unwrap_used)]

use tracedecay_lsp as lsp;

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
    assert!(
        adapter(&adapters, "typescript").install_options[0]
            .command
            .contains("typescript-language-server")
    );
    assert!(
        adapter(&adapters, "rust").install_options[0]
            .command
            .contains("rust-analyzer")
    );
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
        FAKE_LSP_TIMEOUT,
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
            std::time::Duration::from_millis(500),
        )
        .await
        .unwrap_err();

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
    std::fs::write(&script_path, fake_lsp_script_with_partial_publish()).unwrap();
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
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    handle.abort();
    let _ = handle.await;

    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    // The property under test is that aborting a partial refresh does not
    // poison the broker: a *subsequent* refresh must spin up a clean client.
    // On a loaded CI runner the recovery client's `python3` cold-start can
    // exceed the initialize floor and surface a transient "initialize timed
    // out" — that is a slow start, not a poisoned broker — so retry the
    // recovery a bounded number of times before asserting.
    let mut recovery = None;
    for attempt in 0..5 {
        let result = broker
            .refresh_documents(
                FAKE_LANGUAGE,
                vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
                FAKE_LSP_TIMEOUT,
            )
            .await;
        match &result {
            Ok(()) => {
                recovery = Some(result);
                break;
            }
            Err(err) if err.to_string().contains("initialize timed out") => {
                recovery = Some(result);
                if attempt + 1 < 5 {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
            Err(err) => panic!("recovery refresh failed unexpectedly: {err}"),
        }
    }
    recovery
        .expect("recovery refresh should have been attempted")
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
    std::fs::write(
        &script_path,
        fake_lsp_script_that_records_start(&counter_path),
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

    let first_refresh = async move {
        first
            .collect_diagnostics(std::time::Duration::from_millis(500))
            .await
    };
    let second_refresh = async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        second
            .collect_diagnostics(std::time::Duration::from_millis(50))
            .await
    };
    let (first_completed, second_completed) = tokio::join!(first_refresh, second_refresh);

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

fn adapter<'a>(
    adapters: &'a [lsp::adapters::LspAdapterDefinition],
    language: &str,
) -> &'a lsp::adapters::LspAdapterDefinition {
    adapters
        .iter()
        .find(|adapter| adapter.language == language)
        .unwrap_or_else(|| panic!("missing adapter for {language}"))
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
