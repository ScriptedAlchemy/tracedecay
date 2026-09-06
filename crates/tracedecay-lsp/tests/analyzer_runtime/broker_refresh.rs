use tracedecay_lsp::LspSemanticRequestAuthority;

use super::*;

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
    let authority = broker
        .semantic_authority_if_available(
            FAKE_LANGUAGE,
            temp.path().to_path_buf(),
            url::Url::from_directory_path(temp.path())
                .unwrap()
                .to_string(),
            loaded_runner_fake_lsp_timeouts(),
        )
        .unwrap()
        .expect("fake analyzer is executable");
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

    // The aborted task dropped the client it held out of the slot. Its use of
    // the analyzer is concluded on drop, so the shared supervisor stops
    // describing that incarnation as live instead of waiting for the next
    // caller to discover an empty slot; the abort is settled asynchronously,
    // so give it a bounded moment.
    let retired = wait_for_analyzer_state(&authority, AnalyzerState::RestartBackoff).await;
    assert_eq!(retired.last_failure(), Some(AnalyzerEvent::Retired));
    assert_eq!(
        retired.restart_attempts(),
        0,
        "a client the lane retired is not a failure the analyzer caused"
    );
    let abandoned_attempt = retired.attempt();

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
    let recovered = authority.analyzer_readiness();
    assert_eq!(recovered.state(), AnalyzerState::Ready);
    assert_eq!(
        recovered.attempt(),
        abandoned_attempt + 1,
        "the clean client is a new incarnation, not the retired one"
    );
    assert_eq!(recovered.restart_attempts(), 0);
}

/// Polls the shared supervisor until it reports `expected`, bounded by the
/// fake-server phase deadline. Abort settles on the runtime's schedule, so the
/// evidence it leaves cannot be asserted synchronously.
async fn wait_for_analyzer_state(
    authority: &lsp::broker::StdioLspSemanticAuthority,
    expected: AnalyzerState,
) -> AnalyzerSupervisor {
    let deadline = tokio::time::Instant::now() + FAKE_LSP_PHASE_TIMEOUT;
    loop {
        let readiness = authority.analyzer_readiness();
        if readiness.state() == expected {
            return readiness;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "analyzer never reached {expected:?}; last {readiness:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
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
async fn broker_drops_warm_client_when_the_admitted_root_disappears() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let script_path = temp.path().join("warm_fake_lsp.py");
    let counter_path = temp.path().join("starts.txt");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble(
            &format!(
                "with open({:?}, \"a\", encoding=\"utf-8\") as starts:\n    starts.write(\"start\\n\")\n",
                counter_path.display().to_string()
            ),
            FAKE_DIAGNOSTIC_PUBLISH,
        ),
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        &project,
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let document = fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope");

    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![document.clone()],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&counter_path)
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(broker.snapshot().diagnostics.len(), 1);

    std::fs::remove_dir_all(&project).unwrap();
    let error = broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![document.clone()],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .expect_err("removed admitted root must fail closed");
    assert!(
        error
            .to_string()
            .contains("failed to resolve admitted project root")
    );
    assert_eq!(broker.snapshot().diagnostics.len(), 1);

    std::fs::create_dir(&project).unwrap();
    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![document],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&counter_path)
            .unwrap()
            .lines()
            .count(),
        2
    );
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
    std::fs::write(temp.path().join("workspace-a/src/lib.fake"), "let nope").unwrap();
    std::fs::write(temp.path().join("workspace-b/src/lib.fake"), "let nope").unwrap();
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

/// The refresh lane starts, reuses, and retires the same client the semantic
/// lane serves from, so its lifecycle must be visible on the one shared
/// supervisor: a refresh that ends without a publication leaves the analyzer
/// `RestartBackoff` with `Retired` evidence and no budget spent, and the next
/// refresh starts a new incarnation rather than serving on the retired one.
#[tokio::test]
async fn refresh_lane_lifecycle_is_recorded_on_the_shared_supervisor() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("never_publish_lsp.py");
    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble("", NEVER_PUBLISH),
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let authority = broker
        .semantic_authority_if_available(
            FAKE_LANGUAGE,
            temp.path().to_path_buf(),
            url::Url::from_directory_path(temp.path())
                .unwrap()
                .to_string(),
            loaded_runner_fake_lsp_timeouts(),
        )
        .unwrap()
        .expect("fake analyzer is executable");
    assert_eq!(
        authority.analyzer_readiness().state(),
        AnalyzerState::AwaitingStart
    );

    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .expect_err("a refresh with no publication times out");

    let retired = authority.analyzer_readiness();
    assert_eq!(retired.state(), AnalyzerState::RestartBackoff);
    assert_eq!(retired.last_failure(), Some(AnalyzerEvent::Retired));
    assert_eq!(retired.restart_attempts(), 0);
    assert_eq!(
        retired.attempt(),
        1,
        "the refresh lane's start is the first attempt"
    );
    assert!(
        authority.upstream_capabilities().await.is_ok(),
        "a retired analyzer is restartable, not retired for the session"
    );
    let restarted = authority.analyzer_readiness();
    assert_eq!(restarted.state(), AnalyzerState::Ready);
    assert_eq!(restarted.attempt(), 2, "the restart is a new incarnation");
    assert_eq!(restarted.restart_attempts(), 0);

    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .expect_err("the warm client still runs the script that never publishes");
    let retired_again = authority.analyzer_readiness();
    assert_eq!(retired_again.state(), AnalyzerState::RestartBackoff);
    assert_eq!(
        retired_again.attempt(),
        2,
        "the warm client was incarnation two"
    );

    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .expect("a fresh start runs the publishing script");
    let served = authority.analyzer_readiness();
    assert_eq!(served.state(), AnalyzerState::Ready);
    assert_eq!(served.attempt(), 3);
    assert_eq!(
        served.served_requests(),
        1,
        "a refresh the analyzer answered counts as service by this incarnation"
    );
    assert_eq!(served.restart_attempts(), 0);
}

/// Restart exhaustion is a stable health state for the whole shared slot: once
/// the semantic lane has spent the budget on consecutive start failures, the
/// refresh lane does not spawn the analyzer behind its back, and a healthier
/// script on disk changes nothing until the daemon decides otherwise.
#[tokio::test]
async fn an_exhausted_restart_budget_stops_the_refresh_lane_from_respawning() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("never_initialize_lsp.py");
    let starts_path = temp.path().join("starts.txt");
    std::fs::write(
        &script_path,
        format!(
            r#"
import time
with open({starts:?}, "a", encoding="utf-8") as f:
    f.write("start\n")
time.sleep(60)
"#,
            starts = starts_path.display().to_string(),
        ),
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let root_uri = url::Url::from_directory_path(temp.path())
        .unwrap()
        .to_string();
    let authority = broker
        .semantic_authority_if_available(
            FAKE_LANGUAGE,
            temp.path().to_path_buf(),
            root_uri.clone(),
            bounded_fake_lsp_timeouts(),
        )
        .unwrap()
        .expect("fake analyzer is executable");

    for request in 1..=i64::from(tracedecay_lsp::MAX_ANALYZER_RESTARTS) {
        let outcome = authority
            .start(
                AdmittedRoot::new(root_uri.clone()),
                LspRequestId::Number(request),
                tracedecay_lsp::lsp_semantic_request(&SemanticRequest::DocumentSymbols {
                    document_uri: format!("{root_uri}src/lib.fake"),
                })
                .unwrap(),
            )
            .await;
        assert!(
            matches!(
                outcome,
                tracedecay_lsp::LspSemanticOperationOutcome::Partial { ref coverage, .. }
                    if coverage == "analyzer-start-failed"
            ),
            "request {request}: {outcome:?}"
        );
    }
    let exhausted = authority.analyzer_readiness();
    assert_eq!(exhausted.state(), AnalyzerState::Exhausted);
    // Every failed start waits for its child to exit before reporting, so this
    // read races nothing. A starved runner can kill a start before python
    // wrote its line, which is why only the upper bound is exact.
    let starts_before = std::fs::read_to_string(&starts_path).unwrap();
    assert!(starts_before.lines().count() <= usize::from(tracedecay_lsp::MAX_ANALYZER_RESTARTS));

    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    let error = broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .expect_err("the refresh lane must not respawn an exhausted analyzer");

    assert!(
        error.to_string().contains("restart budget exhausted"),
        "unexpected error: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(&starts_path).unwrap(),
        starts_before,
        "no process was started"
    );
    assert_eq!(authority.analyzer_readiness(), exhausted);
    let snapshot = broker.snapshot();
    assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Crashed);
}
