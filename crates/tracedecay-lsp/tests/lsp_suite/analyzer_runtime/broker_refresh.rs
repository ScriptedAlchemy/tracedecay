use tracedecay_lsp::LspSemanticRequestAuthority;

use super::*;

#[test]
fn broker_refresh_documents_populates_cached_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("fake_lsp.py");
    std::fs::write(&script_path, fake_lsp_script()).unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );

    block(broker.refresh_documents(
        FAKE_LANGUAGE,
        vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
        FAKE_LSP_TIMEOUT,
    ))
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

#[cfg(unix)]
async fn assert_abandoned_start_settles_before_successor(cancel: bool) {
    let temp = tempfile::tempdir().unwrap();
    let document_path = temp.path().join(FAKE_PATH);
    std::fs::create_dir_all(document_path.parent().unwrap()).unwrap();
    std::fs::write(&document_path, "valid").unwrap();
    let script_path = temp.path().join("startup_settlement_lsp.py");
    let starts_path = temp.path().join("starts.txt");
    let overlap_path = temp.path().join("overlap.txt");
    std::fs::write(
        &script_path,
        format!(
            r#"
import json
import os
import sys
import time

starts_path = {starts_path:?}
overlap_path = {overlap_path:?}
prior = []
if os.path.exists(starts_path):
    with open(starts_path, encoding="utf-8") as starts:
        prior = [int(line) for line in starts if line.strip()]
for pid in prior:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        continue
    with open(overlap_path, "w", encoding="utf-8") as overlap:
        overlap.write(str(pid))
with open(starts_path, "a", encoding="utf-8") as starts:
    starts.write(str(os.getpid()) + "\n")

def read_message():
    headers = {{}}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        name, value = line.decode("ascii").split(":", 1)
        headers[name.lower()] = value.strip()
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))

def send(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n" + body)
    sys.stdout.buffer.flush()

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        if not prior:
            time.sleep(60)
        send({{"jsonrpc": "2.0", "id": message["id"], "result": {{"capabilities": {{"documentSymbolProvider": True}}}}}})
    elif method == "textDocument/documentSymbol":
        send({{"jsonrpc": "2.0", "id": message["id"], "result": []}})
"#,
            starts_path = starts_path.display().to_string(),
            overlap_path = overlap_path.display().to_string(),
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
            lsp::client::LspRefreshTimeouts::new(
                std::time::Duration::from_secs(1),
                FAKE_LSP_START_TIMEOUT * 2,
                FAKE_LSP_START_TIMEOUT * 2,
                std::time::Duration::from_millis(50),
            ),
        )
        .await
        .unwrap()
        .expect("fake analyzer is executable");
    let first_id = LspRequestId::Number(901);
    let first_request = tracedecay_lsp::lsp_semantic_request(&SemanticRequest::DocumentSymbols {
        document_uri: format!("{root_uri}{FAKE_PATH}"),
    })
    .unwrap();
    let first = cancel.then(|| {
        tokio::spawn({
            let authority = Arc::clone(&authority);
            let root = AdmittedRoot::new(root_uri.clone());
            let first_id = first_id.clone();
            async move { authority.start(root, first_id, first_request).await }
        })
    });
    struct TokioTask(tokio::task::JoinHandle<()>);
    impl tracedecay_lsp::LspRuntimeTask for TokioTask {
        fn abort(&self) {
            self.0.abort();
        }
    }
    struct TokioSpawner;
    impl tracedecay_lsp::LspRuntimeSpawner for TokioSpawner {
        fn spawn(
            &self,
            future: tracedecay_lsp::LspRuntimeFuture<()>,
        ) -> Box<dyn tracedecay_lsp::LspRuntimeTask> {
            Box::new(TokioTask(tokio::spawn(future)))
        }
    }
    let deadline_adapter = (!cancel).then(|| {
        let semantic_authority: Arc<dyn tracedecay_lsp::LspSemanticRequestAuthority> =
            authority.clone();
        tracedecay_lsp::SemanticProviderAdapter::new(Arc::new(TokioSpawner), semantic_authority)
    });
    let semantic_request = SemanticRequest::DocumentSymbols {
        document_uri: format!("{root_uri}{FAKE_PATH}"),
    };
    if let Some(adapter) = deadline_adapter.as_ref() {
        assert!(matches!(
            adapter.request_with_document(
                &AdmittedRoot::new(root_uri.clone()),
                &first_id,
                &semantic_request,
                None,
                Some(std::time::Instant::now() + FAKE_LSP_START_TIMEOUT),
            ),
            tracedecay_lsp::SemanticProviderOutcome::Pending
        ));
    }
    tokio::time::timeout(FAKE_LSP_START_TIMEOUT, async {
        while std::fs::read_to_string(&starts_path)
            .map_or(true, |starts| starts.lines().next().is_none())
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first analyzer must spawn before abandonment");
    if cancel {
        assert!(authority.cancel_request(&AdmittedRoot::new(root_uri.clone()), &first_id));
    }
    if let Some(first) = first {
        let first_outcome = first.await.unwrap();
        assert!(
            matches!(
                first_outcome,
                tracedecay_lsp::LspSemanticOperationOutcome::Partial { ref coverage, .. }
                    if coverage == "analyzer-cancelled"
            ),
            "unexpected abandoned startup outcome: {first_outcome:?}"
        );
    } else {
        let adapter = deadline_adapter.as_ref().unwrap();
        let expired = tokio::time::timeout(FAKE_LSP_START_TIMEOUT * 2, async {
            loop {
                match adapter.request_with_document(
                    &AdmittedRoot::new(root_uri.clone()),
                    &first_id,
                    &semantic_request,
                    None,
                    None,
                ) {
                    tracedecay_lsp::SemanticProviderOutcome::Pending => {
                        tokio::task::yield_now().await;
                    }
                    outcome => return outcome,
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            expired,
            tracedecay_lsp::SemanticProviderOutcome::Partial { ref coverage, .. }
                if coverage == "analyzer-timeout"
        ));
    }

    let second = authority
        .start(
            AdmittedRoot::new(root_uri.clone()),
            LspRequestId::Number(902),
            tracedecay_lsp::lsp_semantic_request(&SemanticRequest::DocumentSymbols {
                document_uri: format!("{root_uri}{FAKE_PATH}"),
            })
            .unwrap(),
        )
        .await;
    assert_eq!(
        second,
        tracedecay_lsp::LspSemanticOperationOutcome::Complete(serde_json::json!([]))
    );
    assert_eq!(
        std::fs::read_to_string(starts_path)
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(
        !overlap_path.exists(),
        "a successor spawned while the abandoned analyzer PID was still alive"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn cancelled_startup_settles_its_spawned_process_before_a_successor() {
    assert_abandoned_start_settles_before_successor(true).await;
}

#[cfg(unix)]
#[tokio::test]
async fn expired_startup_settles_its_spawned_process_before_a_successor() {
    assert_abandoned_start_settles_before_successor(false).await;
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
    let authority = broker
        .semantic_authority_if_available(
            FAKE_LANGUAGE,
            temp.path().to_path_buf(),
            url::Url::from_directory_path(temp.path())
                .unwrap()
                .to_string(),
            hang_after_start_fake_lsp_timeouts(),
        )
        .await
        .unwrap()
        .expect("fake analyzer is executable");

    let result = tokio::time::timeout(
        FAKE_LSP_START_TIMEOUT + OUTER_ASYNC_TIMEOUT,
        broker.refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(
                FAKE_LANGUAGE,
                FAKE_PATH,
                &"let nope\n".repeat(HANGING_WRITE_LINE_COUNT),
            )],
            hang_after_start_fake_lsp_timeouts(),
        ),
    )
    .await;

    let err = result
        .expect("refresh should be bounded while writing document text")
        .expect_err("hung document write should crash the engine");
    assert!(err.to_string().contains("timed out"));
    let snapshot = broker.snapshot();
    assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Crashed);
    let failed = authority.analyzer_readiness();
    assert_eq!(failed.state(), AnalyzerState::RestartBackoff);
    assert_eq!(failed.last_failure(), Some(AnalyzerEvent::TransportFailed));
    assert_eq!(
        failed.restart_attempts(),
        1,
        "a blocked diagnostics transport is an analyzer failure"
    );

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

    assert_eq!(diagnostics.diagnostics.len(), 1);
    assert_eq!(diagnostics.diagnostics[0].message, "fake type error");
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
    let authority = broker
        .semantic_authority_if_available(
            FAKE_LANGUAGE,
            temp.path().to_path_buf(),
            url::Url::from_directory_path(temp.path())
                .unwrap()
                .to_string(),
            hang_after_start_fake_lsp_timeouts(),
        )
        .await
        .unwrap()
        .expect("fake analyzer is executable");

    let err = broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            hang_after_start_fake_lsp_timeouts(),
        )
        .await
        .expect_err("partial diagnostics frame should crash the cached client");
    assert!(err.to_string().contains("timed out"));
    let snapshot = broker.snapshot();
    assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Crashed);
    let failed = authority.analyzer_readiness();
    assert_eq!(failed.state(), AnalyzerState::RestartBackoff);
    assert_eq!(failed.last_failure(), Some(AnalyzerEvent::InvalidResponse));
    assert_eq!(
        failed.restart_attempts(),
        1,
        "an incomplete protocol frame is an analyzer failure"
    );

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

    let timeouts = lsp::client::LspRefreshTimeouts::from_diagnostics_quiet_window(
        std::time::Duration::from_millis(50),
    );
    let mut client = lsp::client::StdioLspClient::start_with_timeouts(
        python_command(),
        &[script_path.display().to_string()],
        temp.path(),
        timeouts,
    )
    .await
    .unwrap();
    let err = client
        .collect_document_diagnostics(
            temp.path(),
            vec![
                fake_document(FAKE_LANGUAGE, "src/first.fake", "let nope"),
                fake_document(FAKE_LANGUAGE, "src/second.fake", "let clean"),
            ],
            timeouts,
        )
        .await
        .expect_err("a batch with zero publishes should fail as a timeout");

    assert!(
        matches!(err, lsp::client::LspRefreshError::NoPublication { .. }),
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

    assert_eq!(diagnostics.diagnostics.len(), 1);
    assert_eq!(diagnostics.diagnostics[0].file, "src/first.fake");
    assert_eq!(diagnostics.diagnostics[0].message, "first file error");
    assert_eq!(
        diagnostics.coverage.documents.get("src/first.fake"),
        Some(&lsp::client::DiagnosticDocumentCoverage::Published {
            version_verified: false,
            content_digest: tracedecay_domain::ContentDigest::of_bytes(b"let nope"),
        })
    );
    assert_eq!(
        diagnostics.coverage.documents.get("src/second.fake"),
        Some(&lsp::client::DiagnosticDocumentCoverage::NoPublication)
    );
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
        .await
        .unwrap()
        .expect("fake analyzer is executable");
    let cancellations = usize::from(tracedecay_lsp::MAX_ANALYZER_RESTARTS) + 1;
    let mut abandoned_attempt = 0;
    for cancellation in 1..=cancellations {
        let prepared = broker
            .prepare_refresh(
                FAKE_LANGUAGE,
                vec![fake_document(
                    FAKE_LANGUAGE,
                    "src/canceled.fake",
                    "let nope",
                )],
            )
            .await
            .unwrap()
            .expect("refresh should prepare");
        let handle = tokio::spawn(async move {
            prepared
                .collect_diagnostics_with_timeouts(phase_gated_fake_lsp_timeouts())
                .await
        });
        control
            .wait_for("partial-frame-flushed")
            .await
            .release()
            .await;
        handle.abort();
        let _ = handle.await;

        // The aborted task drops a client whose stream is parked mid-frame.
        // The shared slot must retire and reap it before the next iteration
        // starts a successor, without charging a failure to the analyzer.
        let retired = wait_for_analyzer_state(&authority, AnalyzerState::RestartBackoff).await;
        assert_eq!(retired.last_failure(), Some(AnalyzerEvent::Retired));
        assert_eq!(
            retired.restart_attempts(),
            0,
            "cancellation {cancellation} must not spend the analyzer restart budget"
        );
        assert!(
            retired.attempt() > abandoned_attempt,
            "each cancelled refresh owns a new incarnation"
        );
        abandoned_attempt = retired.attempt();
    }

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
        .await
        .unwrap()
        .expect("first refresh should prepare");
    let second = broker
        .prepare_refresh(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
        )
        .await
        .unwrap()
        .expect("second refresh should prepare");

    let first_refresh = tokio::spawn(async move {
        first
            .collect_diagnostics_with_timeouts(phase_gated_fake_lsp_timeouts())
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
        .await
        .unwrap()
        .expect("stale refresh should prepare");
    let latest = broker
        .prepare_refresh(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, "src/latest.fake", "let nope")],
        )
        .await
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
                "import os\nos.chdir({:?})\nwith open({:?}, \"a\", encoding=\"utf-8\") as starts:\n    starts.write(\"start\\n\")\n",
                temp.path().display().to_string(),
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
        .await
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

/// A diagnostics quiet-window decision is not evidence that the analyzer
/// crashed. Repeating it beyond the restart budget must remain restartable,
/// and the next capability read plus diagnostic refresh must share one healthy
/// incarnation.
#[tokio::test]
async fn refresh_quiet_window_stays_budget_neutral_and_reuses_live_client() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("never_publish_lsp.py");
    let starts_path = temp.path().join("starts.txt");
    let preamble = format!(
        r#"
with open({starts:?}, "a", encoding="utf-8") as f:
    f.write("start\n")
"#,
        starts = starts_path.display().to_string(),
    );
    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble(&preamble, NEVER_PUBLISH),
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
        .await
        .unwrap()
        .expect("fake analyzer is executable");
    assert_eq!(
        authority.analyzer_readiness().state(),
        AnalyzerState::AwaitingStart
    );

    let quiet_retirements = usize::from(tracedecay_lsp::MAX_ANALYZER_RESTARTS) + 1;
    for attempt in 1..=quiet_retirements {
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
        assert_eq!(
            retired.restart_attempts(),
            0,
            "quiet-window retirement {attempt} must not spend the restart budget"
        );
        assert_eq!(retired.attempt(), u32::try_from(attempt).unwrap());
    }
    assert_engine_state(
        &broker.snapshot(),
        FAKE_LANGUAGE,
        lsp::broker::EngineState::Available,
    );

    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble(&preamble, FAKE_DIAGNOSTIC_PUBLISH),
    )
    .unwrap();
    assert!(
        authority.upstream_capabilities().await.is_ok(),
        "a quiet-window retirement remains restartable"
    );
    let capability_attempt = authority.analyzer_readiness().attempt();
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
    assert_eq!(
        served.attempt(),
        capability_attempt,
        "capability negotiation and diagnostics use the same live client"
    );
    assert_eq!(
        served.served_requests(),
        1,
        "a refresh the analyzer answered counts as service by this incarnation"
    );
    assert_eq!(served.restart_attempts(), 0);
    let starts = std::fs::read_to_string(starts_path).unwrap();
    assert_eq!(starts.lines().count(), quiet_retirements + 1);
}

/// A process that initializes and then exits during diagnostics is a genuine
/// analyzer failure. Refreshes alone must spend the shared budget and leave
/// both semantic capability reads and diagnostics starts terminally refused.
#[tokio::test]
async fn refresh_only_post_initialize_crash_loop_exhausts_restart_budget() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("exit_after_initialize_lsp.py");
    let starts_path = temp.path().join("starts.txt");
    let preamble = format!(
        r#"
with open({starts:?}, "a", encoding="utf-8") as f:
    f.write("start\n")
"#,
        starts = starts_path.display().to_string(),
    );
    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble(&preamble, "        sys.exit(17)\n"),
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
        .await
        .unwrap()
        .expect("fake analyzer is executable");

    for failure in 1..=tracedecay_lsp::MAX_ANALYZER_RESTARTS {
        let error = broker
            .refresh_documents_with_timeouts(
                FAKE_LANGUAGE,
                vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
                loaded_runner_fake_lsp_timeouts(),
            )
            .await
            .expect_err("the post-initialize process exit must fail the refresh");
        assert!(
            error.to_string().contains("exited")
                || error
                    .to_string()
                    .contains("closed its diagnostics transport"),
            "unexpected error: {error}"
        );

        let readiness = authority.analyzer_readiness();
        let expected = if failure == tracedecay_lsp::MAX_ANALYZER_RESTARTS {
            AnalyzerState::Exhausted
        } else {
            AnalyzerState::RestartBackoff
        };
        assert_eq!(readiness.state(), expected);
        assert!(matches!(
            readiness.last_failure(),
            Some(AnalyzerEvent::Crashed | AnalyzerEvent::TransportFailed)
        ));
        assert_eq!(readiness.restart_attempts(), failure);
    }

    assert!(matches!(
        authority.upstream_capabilities().await,
        Err(lsp::AnalyzerRuntimeError::Unavailable)
    ));
    let starts_before = std::fs::read_to_string(&starts_path).unwrap();
    assert_eq!(
        starts_before.lines().count(),
        usize::from(tracedecay_lsp::MAX_ANALYZER_RESTARTS)
    );
    let error = broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "let nope")],
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .expect_err("an exhausted refresh-only loop must refuse another start");
    assert!(error.to_string().contains("restart budget exhausted"));
    assert_eq!(
        std::fs::read_to_string(starts_path).unwrap(),
        starts_before,
        "terminal refusal must not start another process"
    );
    assert_engine_state(
        &broker.snapshot(),
        FAKE_LANGUAGE,
        lsp::broker::EngineState::Crashed,
    );
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
    std::fs::write(&starts_path, "").unwrap();
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
        .await
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

#[tokio::test]
async fn semantic_receive_dispatch_keeps_equal_bidirectional_ids_distinct() {
    let temp = tempfile::tempdir().unwrap();
    let document_path = temp.path().join(FAKE_PATH);
    std::fs::create_dir_all(document_path.parent().unwrap()).unwrap();
    std::fs::write(&document_path, "let value = 1;").unwrap();
    let script_path = temp.path().join("bidirectional_lsp.py");
    let reply_path = temp.path().join("apply_edit_reply.json");
    std::fs::write(
        &script_path,
        format!(
            r#"
import json
import sys

def read_message():
    headers = {{}}
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
        send({{"jsonrpc": "2.0", "id": message["id"], "result": {{"capabilities": {{}}}}}})
    elif message.get("method") == "textDocument/documentSymbol":
        request_id = message["id"]
        send({{
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "workspace/applyEdit",
            "params": {{"edit": {{"changes": {{}}}}}},
        }})
        reply = read_message()
        with open({reply_path:?}, "w", encoding="utf-8") as output:
            json.dump(reply, output)
        send({{"jsonrpc": "2.0", "id": request_id, "result": []}})
        break
"#,
            reply_path = reply_path.display().to_string(),
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
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap()
        .expect("fake analyzer is executable");

    let outcome = authority
        .start(
            AdmittedRoot::new(root_uri.clone()),
            LspRequestId::Number(2),
            tracedecay_lsp::lsp_semantic_request(&SemanticRequest::DocumentSymbols {
                document_uri: format!("{root_uri}{FAKE_PATH}"),
            })
            .unwrap(),
        )
        .await;

    assert_eq!(
        outcome,
        tracedecay_lsp::LspSemanticOperationOutcome::Complete(serde_json::json!([]))
    );
    let reply: serde_json::Value =
        serde_json::from_slice(&std::fs::read(reply_path).unwrap()).unwrap();
    assert_eq!(reply["id"], 2);
    assert_eq!(reply["result"]["applied"], false);
}

#[tokio::test]
async fn semantic_eof_is_retired_as_transport_failure_not_timeout() {
    assert_semantic_eof_outcome(false).await;
}

#[tokio::test]
async fn semantic_cancellation_after_eof_releases_the_registered_operation() {
    assert_semantic_eof_outcome(true).await;
}

async fn assert_semantic_eof_outcome(cancel_after_eof: bool) {
    let temp = tempfile::tempdir().unwrap();
    let document_path = temp.path().join(FAKE_PATH);
    std::fs::create_dir_all(document_path.parent().unwrap()).unwrap();
    std::fs::write(&document_path, "let value = 1;").unwrap();
    let script_path = temp.path().join("closed_stdout_lsp.py");
    std::fs::write(
        &script_path,
        r#"
import json
import os
from pathlib import Path
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

while True:
    message = read_message()
    if message is None:
        break
    if message.get("method") == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {}}})
    elif message.get("method") == "textDocument/documentSymbol":
        os.close(sys.stdout.fileno())
        Path(__file__).with_suffix(".closed").write_text("closed")
        time.sleep(60)
"#,
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
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap()
        .expect("fake analyzer is executable");

    let operation = authority.start(
        AdmittedRoot::new(root_uri.clone()),
        LspRequestId::Number(3),
        tracedecay_lsp::lsp_semantic_request(&SemanticRequest::DocumentSymbols {
            document_uri: format!("{root_uri}{FAKE_PATH}"),
        })
        .unwrap(),
    );
    let outcome = if cancel_after_eof {
        let operation = tokio::spawn(operation);
        tokio::time::timeout(FAKE_LSP_RECOVERY_TIMEOUT, async {
            while !script_path.with_extension("closed").exists() {
                tokio::task::yield_now().await;
            }
            assert!(authority.cancel_request(
                &AdmittedRoot::new(root_uri.clone()),
                &LspRequestId::Number(3),
            ));
            operation.await.unwrap()
        })
        .await
        .expect("cancellation after EOF must not wait for the response deadline")
    } else {
        operation.await
    };
    if cancel_after_eof {
        assert_eq!(
            outcome,
            tracedecay_lsp::LspSemanticOperationOutcome::Partial {
                value: serde_json::Value::Null,
                coverage: "analyzer-cancelled".to_owned(),
                detail: Some(
                    tracedecay_lsp::LspSemanticOperationOutcome::ANALYZER_CANCELLED_DETAIL,
                ),
            },
        );
        assert!(!authority.cancel_request(&AdmittedRoot::new(root_uri), &LspRequestId::Number(3),));
        return;
    }

    assert!(matches!(
        outcome,
        tracedecay_lsp::LspSemanticOperationOutcome::Partial { ref coverage, .. }
            if coverage == "analyzer-transport-failed"
    ));
    assert_eq!(
        authority.analyzer_readiness().last_failure(),
        Some(AnalyzerEvent::TransportFailed)
    );
}

#[tokio::test]
async fn semantic_process_exit_is_classified_before_the_response_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let document_path = temp.path().join(FAKE_PATH);
    std::fs::create_dir_all(document_path.parent().unwrap()).unwrap();
    std::fs::write(&document_path, "let value = 1;").unwrap();
    let script_path = temp.path().join("exit_during_semantic_lsp.py");
    std::fs::write(
        &script_path,
        r#"
import json
import os
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

while True:
    message = read_message()
    if message is None:
        break
    if message.get("method") == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {}}})
    elif message.get("method") == "textDocument/documentSymbol":
        os.close(sys.stdout.fileno())
        time.sleep(0.1)
        sys.exit(17)
"#,
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
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap()
        .expect("fake analyzer is executable");

    let outcome = authority
        .start(
            AdmittedRoot::new(root_uri.clone()),
            LspRequestId::Number(31),
            tracedecay_lsp::lsp_semantic_request(&SemanticRequest::DocumentSymbols {
                document_uri: format!("{root_uri}{FAKE_PATH}"),
            })
            .unwrap(),
        )
        .await;

    assert!(matches!(
        outcome,
        tracedecay_lsp::LspSemanticOperationOutcome::Partial { ref coverage, .. }
            if coverage == "analyzer-crashed"
    ));
    assert_eq!(
        authority.analyzer_readiness().last_failure(),
        Some(AnalyzerEvent::Crashed)
    );
}

#[tokio::test]
async fn semantic_gateway_preserves_overlay_identity_until_its_session_closes() {
    use tracedecay_lsp::{
        ClientCapabilities, DaemonLspGateway, FeedbackCyclePort, FeedbackCycleRequest,
        FeedbackCycleResponse, GatewayCapabilities, GatewayResponse, LspRuntimeFuture,
        LspRuntimeSpawner, LspRuntimeTask, LspSemanticDocumentSnapshot, SemanticCapability,
        SemanticProviderAdapter, SemanticResponse, UpstreamCapabilities, negotiate_capabilities,
    };

    struct NoopFeedback;

    impl FeedbackCyclePort for NoopFeedback {
        fn request_feedback_cycle(&self, _request: FeedbackCycleRequest) -> FeedbackCycleResponse {
            FeedbackCycleResponse::Deferred {
                reason: "test-not-configured".to_owned(),
            }
        }
    }

    struct TokioTask(tokio::task::JoinHandle<()>);

    impl LspRuntimeTask for TokioTask {
        fn abort(&self) {
            self.0.abort();
        }
    }

    struct TokioSpawner;

    impl LspRuntimeSpawner for TokioSpawner {
        fn spawn(&self, future: LspRuntimeFuture<()>) -> Box<dyn LspRuntimeTask> {
            Box::new(TokioTask(tokio::spawn(future)))
        }
    }

    async fn complete_semantic_request<P, S>(
        gateway: &DaemonLspGateway<P, S>,
        request_id: &LspRequestId,
        request: &SemanticRequest,
        snapshot: Option<&LspSemanticDocumentSnapshot>,
    ) -> SemanticResponse
    where
        P: FeedbackCyclePort,
        S: SemanticProviderPort,
    {
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match gateway.semantic_request_with_document(request_id, request, snapshot, None) {
                    GatewayResponse::Pending => tokio::task::yield_now().await,
                    GatewayResponse::Value(value) => return value,
                    other => panic!("semantic request did not complete: {other:?}"),
                }
            }
        })
        .await
        .expect("semantic request should complete")
    }

    let temp = tempfile::tempdir().unwrap();
    let document_path = temp.path().join(FAKE_PATH);
    std::fs::create_dir_all(document_path.parent().unwrap()).unwrap();
    std::fs::write(&document_path, "stale disk text").unwrap();
    let script_path = temp.path().join("snapshot_lsp.py");
    let events_path = temp.path().join("events.jsonl");
    let request_arrived = temp.path().join("request-arrived");
    let release_request = temp.path().join("release-request");
    std::fs::write(
        &script_path,
        format!(
            r#"
import json
import os
import sys
import time

documents = {{}}
versions = {{}}

def read_message():
    headers = {{}}
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

def record(event):
    event["pid"] = os.getpid()
    with open({events_path:?}, "a", encoding="utf-8") as output:
        output.write(json.dumps(event) + "\n")

def publish(uri):
    send({{
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {{"uri": uri, "version": versions[uri], "diagnostics": []}}
    }})

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send({{"jsonrpc": "2.0", "id": message["id"], "result": {{"capabilities": {{}}}}}})
    elif method == "textDocument/didOpen":
        document = message["params"]["textDocument"]
        documents[document["uri"]] = document["text"]
        versions[document["uri"]] = document["version"]
        record({{"method": "open", "uri": document["uri"], "text": document["text"]}})
        publish(document["uri"])
    elif method == "textDocument/didChange":
        document = message["params"]["textDocument"]
        text = message["params"]["contentChanges"][0]["text"]
        documents[document["uri"]] = text
        versions[document["uri"]] = document["version"]
        record({{"method": "change", "text": text, "version": document["version"]}})
        publish(document["uri"])
    elif method == "textDocument/documentSymbol":
        uri = message["params"]["textDocument"]["uri"]
        text = documents.get(uri, "missing document")
        record({{"method": "request", "text": text}})
        if text == "close during request":
            with open({request_arrived:?}, "w", encoding="utf-8") as signal:
                signal.write("ready")
            while not os.path.exists({release_request:?}):
                time.sleep(0.001)
        send({{
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": [{{
                "name": text,
                "kind": 13,
                "range": {{"start": {{"line": 0, "character": 0}}, "end": {{"line": 0, "character": 1}}}},
                "selectionRange": {{"start": {{"line": 0, "character": 0}}, "end": {{"line": 0, "character": 1}}}}
            }}]
        }})
        publish(uri)
"#,
            events_path = events_path.display().to_string(),
            request_arrived = request_arrived.display().to_string(),
            release_request = release_request.display().to_string(),
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
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap()
        .expect("fake analyzer is executable");
    let upstream = UpstreamCapabilities {
        supports_diagnostics: true,
        semantic: SemanticCapability::ALL.into_iter().collect(),
    };
    let client = ClientCapabilities {
        semantic: SemanticCapability::ALL.into_iter().collect(),
        ..ClientCapabilities::default()
    };
    let capabilities = negotiate_capabilities(&client, &GatewayCapabilities::default(), &upstream);
    let semantic_authority: Arc<dyn tracedecay_lsp::LspSemanticRequestAuthority> =
        authority.clone();
    let semantic = SemanticProviderAdapter::new(Arc::new(TokioSpawner), semantic_authority);
    let gateway = DaemonLspGateway::new(
        AdmittedRoot::new(root_uri.clone()),
        capabilities,
        NoopFeedback,
        semantic,
    );
    #[cfg(unix)]
    let presentation_root = {
        let alias = temp.path().join("root-alias");
        std::os::unix::fs::symlink(temp.path(), &alias).unwrap();
        url::Url::from_directory_path(alias).unwrap().to_string()
    };
    #[cfg(not(unix))]
    let presentation_root = root_uri.clone();
    // URI spelling is presentation, not filesystem identity. On Unix the
    // admitted spelling also crosses a root alias.
    let uri = format!(
        "{presentation_root}{}",
        FAKE_PATH.replacen("src", "%73rc", 1)
    );
    let mut overlays = tracedecay_lsp::OverlayStore::default();
    let initial = overlays
        .open(
            &AdmittedRoot::new(root_uri.clone()),
            uri.clone(),
            "fake",
            41,
            "unsaved snapshot one",
        )
        .unwrap();
    let request = SemanticRequest::DocumentSymbols {
        document_uri: uri.clone(),
    };
    let result = complete_semantic_request(
        &gateway,
        &LspRequestId::Number(11),
        &request,
        Some(&LspSemanticDocumentSnapshot::from_overlay(&initial)),
    )
    .await;
    assert!(matches!(
        result,
        SemanticResponse::DocumentSymbols(ref symbols)
            if symbols.first().is_some_and(|symbol| symbol.name == "unsaved snapshot one")
    ));

    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "stale disk text")],
            recovery_fake_lsp_timeouts(),
        )
        .await
        .expect("a disk sweep must collect against the admitted overlay");
    assert_eq!(
        broker.snapshot().coverage[FAKE_LANGUAGE].documents[FAKE_PATH],
        lsp::client::DiagnosticDocumentCoverage::Published {
            version_verified: true,
            content_digest: tracedecay_domain::ContentDigest::of_bytes(b"unsaved snapshot one"),
        }
    );

    let same = LspSemanticDocumentSnapshot::from_overlay(&initial);
    let result =
        complete_semantic_request(&gateway, &LspRequestId::Number(12), &request, Some(&same)).await;
    assert!(matches!(
        result,
        SemanticResponse::DocumentSymbols(ref symbols)
            if symbols.first().is_some_and(|symbol| symbol.name == "unsaved snapshot one")
    ));

    let changed = overlays
        .change(
            &uri,
            42,
            &[tracedecay_lsp::OverlayChange {
                range: None,
                range_length: None,
                text: "unsaved snapshot two".to_owned(),
            }],
        )
        .unwrap();
    let mut diagnostic_document = fake_document(FAKE_LANGUAGE, FAKE_PATH, changed.text.as_ref());
    diagnostic_document.snapshot = Some(LspSemanticDocumentSnapshot::from_overlay(&changed));
    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![diagnostic_document],
            recovery_fake_lsp_timeouts(),
        )
        .await
        .expect("diagnostics may advance the shared ledger to a newer admitted overlay");
    let result = complete_semantic_request(
        &gateway,
        &LspRequestId::Number(13),
        &request,
        Some(&LspSemanticDocumentSnapshot::from_overlay(&changed)),
    )
    .await;
    assert!(matches!(
        result,
        SemanticResponse::DocumentSymbols(ref symbols)
            if symbols.first().is_some_and(|symbol| symbol.name == "unsaved snapshot two")
    ));

    let events = std::fs::read_to_string(&events_path).unwrap();
    let events = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["method"] == "open")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["method"] == "change")
            .map(|event| event["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["unsaved snapshot two"]
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["method"] == "change")
            .map(|event| event["version"].as_i64().unwrap())
            .collect::<Vec<_>>(),
        [2],
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["method"] == "request")
            .map(|event| event["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "unsaved snapshot one",
            "unsaved snapshot one",
            "unsaved snapshot two"
        ]
    );

    overlays.close(&uri).unwrap();
    let reopened = overlays
        .open(
            &AdmittedRoot::new(root_uri.clone()),
            uri.clone(),
            "fake",
            1,
            "reopened snapshot",
        )
        .unwrap();
    let reopened_snapshot = LspSemanticDocumentSnapshot::from_overlay(&reopened);
    let result = complete_semantic_request(
        &gateway,
        &LspRequestId::Number(14),
        &request,
        Some(&reopened_snapshot),
    )
    .await;
    assert!(
        matches!(
            result,
            SemanticResponse::DocumentSymbols(ref symbols)
                if symbols.first().is_some_and(|symbol| symbol.name == "reopened snapshot")
        ),
        "editor version 1 belongs to the reopened lifetime, not version 42's lifetime"
    );
    let healthy = authority.analyzer_readiness();
    assert_eq!(healthy.state(), AnalyzerState::Ready);
    assert_eq!(healthy.restart_attempts(), 0);
    assert_eq!(healthy.last_failure(), None);

    let mut digest_invalid = reopened_snapshot.clone();
    digest_invalid.content_digest = tracedecay_domain::ContentDigest::of_bytes(b"wrong bytes");
    let invalid = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match gateway.semantic_request_with_document(
                &LspRequestId::Number(141),
                &request,
                Some(&digest_invalid),
                None,
            ) {
                GatewayResponse::Pending => tokio::task::yield_now().await,
                outcome => return outcome,
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(invalid, GatewayResponse::Partial { ref coverage, .. }
            if coverage == "analyzer-invalid-response"),
        "digest-invalid local work is rejected without reaching the analyzer: {invalid:?}"
    );
    let after_invalid = authority.analyzer_readiness();
    assert_eq!(after_invalid.state(), AnalyzerState::Ready);
    assert_eq!(after_invalid.attempt(), healthy.attempt());
    assert_eq!(after_invalid.restart_attempts(), healthy.restart_attempts());
    assert_eq!(after_invalid.last_failure(), healthy.last_failure());
    let result = complete_semantic_request(
        &gateway,
        &LspRequestId::Number(142),
        &request,
        Some(&reopened_snapshot),
    )
    .await;
    assert!(matches!(
        result,
        SemanticResponse::DocumentSymbols(ref symbols)
            if symbols.first().is_some_and(|symbol| symbol.name == "reopened snapshot")
    ));

    let advanced = overlays
        .change(
            &uri,
            2,
            &[tracedecay_lsp::OverlayChange {
                range: None,
                range_length: None,
                text: "advanced reopened snapshot".to_owned(),
            }],
        )
        .unwrap();
    let advanced_snapshot = LspSemanticDocumentSnapshot::from_overlay(&advanced);
    let result = complete_semantic_request(
        &gateway,
        &LspRequestId::Number(143),
        &request,
        Some(&advanced_snapshot),
    )
    .await;
    assert!(matches!(
        result,
        SemanticResponse::DocumentSymbols(ref symbols)
            if symbols.first().is_some_and(|symbol| symbol.name == "advanced reopened snapshot")
    ));
    let stale = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match gateway.semantic_request_with_document(
                &LspRequestId::Number(144),
                &request,
                Some(&reopened_snapshot),
                None,
            ) {
                GatewayResponse::Pending => tokio::task::yield_now().await,
                outcome => return outcome,
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(stale, GatewayResponse::Partial { ref coverage, .. }
            if coverage == "analyzer-invalid-response"),
        "an older snapshot in the open lifetime is rejected locally: {stale:?}"
    );
    let after_stale = authority.analyzer_readiness();
    assert_eq!(after_stale.state(), AnalyzerState::Ready);
    assert_eq!(after_stale.attempt(), healthy.attempt());
    assert_eq!(after_stale.restart_attempts(), healthy.restart_attempts());
    assert_eq!(after_stale.last_failure(), healthy.last_failure());
    let result = complete_semantic_request(
        &gateway,
        &LspRequestId::Number(145),
        &request,
        Some(&advanced_snapshot),
    )
    .await;
    assert!(matches!(
        result,
        SemanticResponse::DocumentSymbols(ref symbols)
            if symbols.first().is_some_and(|symbol| symbol.name == "advanced reopened snapshot")
    ));

    let closed_snapshot = LspSemanticDocumentSnapshot::from_overlay(&changed);
    let closed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match gateway.semantic_request_with_document(
                &LspRequestId::Number(15),
                &request,
                Some(&closed_snapshot),
                None,
            ) {
                GatewayResponse::Pending => tokio::task::yield_now().await,
                outcome => return outcome,
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(closed, GatewayResponse::Partial { ref coverage, .. }
        if coverage == "analyzer-cancelled"),
        "queued work from a closed lifetime cannot overwrite the reopened buffer: {closed:?}"
    );
    let after_closed = authority.analyzer_readiness();
    assert_eq!(after_closed.state(), AnalyzerState::Ready);
    assert_eq!(after_closed.attempt(), healthy.attempt());
    assert_eq!(after_closed.restart_attempts(), healthy.restart_attempts());
    assert_eq!(after_closed.last_failure(), healthy.last_failure());
    let result = complete_semantic_request(
        &gateway,
        &LspRequestId::Number(16),
        &request,
        Some(&advanced_snapshot),
    )
    .await;
    assert!(matches!(
        result,
        SemanticResponse::DocumentSymbols(ref symbols)
            if symbols.first().is_some_and(|symbol| symbol.name == "advanced reopened snapshot")
    ));
    let events = std::fs::read_to_string(&events_path).unwrap();
    let pids = events
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["pid"]
                .as_i64()
                .unwrap()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        pids.len(),
        1,
        "local rejection must retain the same analyzer PID"
    );

    overlays.close(&uri).unwrap();
    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "stale disk text")],
            recovery_fake_lsp_timeouts(),
        )
        .await
        .expect("closing the overlay returns the shared analyzer to disk authority");
    assert_eq!(
        broker.snapshot().coverage[FAKE_LANGUAGE].documents[FAKE_PATH],
        lsp::client::DiagnosticDocumentCoverage::Published {
            version_verified: true,
            content_digest: tracedecay_domain::ContentDigest::of_bytes(b"stale disk text"),
        }
    );
    let events = std::fs::read_to_string(&events_path).unwrap();
    let events = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .find(|event| event["method"] == "open")
            .unwrap()["uri"],
        uri
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["method"] == "change")
            .map(|event| event["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "unsaved snapshot two",
            "reopened snapshot",
            "advanced reopened snapshot"
        ]
    );
    assert_eq!(events.last().unwrap()["text"], "stale disk text");

    let closing = overlays
        .open(
            &AdmittedRoot::new(root_uri.clone()),
            uri.clone(),
            "fake",
            1,
            "close during request",
        )
        .unwrap();
    let closing_snapshot = LspSemanticDocumentSnapshot::from_overlay(&closing);
    assert!(matches!(
        gateway.semantic_request_with_document(
            &LspRequestId::Number(17),
            &request,
            Some(&closing_snapshot),
            None,
        ),
        GatewayResponse::Pending
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !request_arrived.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("analyzer must receive the request before its overlay closes");
    overlays.close(&uri).unwrap();
    let surviving = overlays
        .open(
            &AdmittedRoot::new(root_uri),
            uri.clone(),
            "fake",
            1,
            "surviving reopened snapshot",
        )
        .unwrap();
    std::fs::write(release_request, "continue").unwrap();
    let cancelled = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            match gateway.semantic_request_with_document(
                &LspRequestId::Number(17),
                &request,
                Some(&closing_snapshot),
                None,
            ) {
                GatewayResponse::Pending => tokio::task::yield_now().await,
                outcome => return outcome,
            }
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(cancelled, GatewayResponse::Partial { ref coverage, .. }
        if coverage == "analyzer-cancelled"),
        "a response arriving after close cannot publish its former overlay: {cancelled:?}"
    );
    let result = complete_semantic_request(
        &gateway,
        &LspRequestId::Number(18),
        &request,
        Some(&LspSemanticDocumentSnapshot::from_overlay(&surviving)),
    )
    .await;
    assert!(
        matches!(result, SemanticResponse::DocumentSymbols(ref symbols)
        if symbols.first().is_some_and(|symbol| symbol.name == "surviving reopened snapshot"))
    );
}

#[test]
fn semantic_request_synchronizes_saved_edits_without_a_diagnostic_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let document_path = temp.path().join(FAKE_PATH);
    std::fs::create_dir_all(document_path.parent().unwrap()).unwrap();
    std::fs::write(&document_path, "saved version one").unwrap();
    let script_path = temp.path().join("saved_document_lsp.py");
    let events_path = temp.path().join("saved_events.jsonl");
    std::fs::write(
        &script_path,
        format!(
            r#"
import json
import sys

documents = {{}}

def read_message():
    headers = {{}}
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

def record(method, text):
    with open({events_path:?}, "a", encoding="utf-8") as output:
        output.write(json.dumps({{"method": method, "text": text}}) + "\n")

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send({{"jsonrpc": "2.0", "id": message["id"], "result": {{"capabilities": {{}}}}}})
    elif method == "textDocument/didOpen":
        document = message["params"]["textDocument"]
        documents[document["uri"]] = document["text"]
        record("open", document["text"])
    elif method == "textDocument/didChange":
        document = message["params"]["textDocument"]
        text = message["params"]["contentChanges"][0]["text"]
        documents[document["uri"]] = text
        record("change", text)
    elif method == "textDocument/documentSymbol":
        uri = message["params"]["textDocument"]["uri"]
        text = documents.get(uri, "missing document")
        record("request", text)
        send({{
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": [{{
                "name": text,
                "kind": 13,
                "range": {{"start": {{"line": 0, "character": 0}}, "end": {{"line": 0, "character": 1}}}},
                "selectionRange": {{"start": {{"line": 0, "character": 0}}, "end": {{"line": 0, "character": 1}}}}
            }}]
        }})
"#,
            events_path = events_path.display().to_string(),
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
    let authority = block(broker.semantic_authority_if_available(
        FAKE_LANGUAGE,
        temp.path().to_path_buf(),
        root_uri.clone(),
        recovery_fake_lsp_timeouts(),
    ))
    .unwrap()
    .expect("fake analyzer is executable");
    let root = AdmittedRoot::new(root_uri.clone());
    let request = || {
        tracedecay_lsp::lsp_semantic_request(&SemanticRequest::DocumentSymbols {
            document_uri: format!("{root_uri}{FAKE_PATH}"),
        })
        .unwrap()
    };
    for (request_id, expected) in [(21, "saved version one"), (22, "saved version two")] {
        if request_id == 22 {
            std::fs::write(&document_path, expected).unwrap();
        }
        let outcome =
            block(authority.start(root.clone(), LspRequestId::Number(request_id), request()));
        let tracedecay_lsp::LspSemanticOperationOutcome::Complete(value) = outcome else {
            panic!("semantic request was incomplete: {outcome:?}");
        };
        assert_eq!(value[0]["name"], expected);
    }
    let missing = block(
        authority.start(
            root,
            LspRequestId::Number(23),
            tracedecay_lsp::lsp_semantic_request(&SemanticRequest::DocumentSymbols {
                document_uri: format!("{root_uri}src/missing.fake"),
            })
            .unwrap(),
        ),
    );
    assert!(matches!(
        missing,
        tracedecay_lsp::LspSemanticOperationOutcome::Partial { ref coverage, .. }
            if coverage == "analyzer-invalid-response"
    ));

    let events = std::fs::read_to_string(events_path).unwrap();
    let events = events
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["method"] == "open")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["method"] == "change")
            .map(|event| event["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["saved version two"]
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["method"] == "request")
            .map(|event| event["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["saved version one", "saved version two"]
    );
}

#[tokio::test]
async fn partial_diagnostic_publications_clear_only_explicitly_reported_documents() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("partial_diagnostics_lsp.py");
    std::fs::write(
        &script_path,
        r#"
import json
import sys

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

def publish(uri, diagnostics):
    send({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "diagnostics": diagnostics}
    })

def publish_version(uri, version, diagnostics):
    send({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {"uri": uri, "version": version, "diagnostics": diagnostics}
    })

def finding(message):
    return {
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": 0, "character": 1}
        },
        "severity": 1,
        "message": message,
        "source": "fake-ls"
    }

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {}}})
    elif method == "textDocument/didOpen":
        uri = message["params"]["textDocument"]["uri"]
        publish(uri, [finding("initial finding")])
    elif method == "textDocument/didChange":
        uri = message["params"]["textDocument"]["uri"]
        text = message["params"]["contentChanges"][0]["text"]
        if uri.endswith("a.fake"):
            publish_version(uri, message["params"]["textDocument"]["version"], [])
        elif text == "stale b":
            publish_version(uri, 1, [finding("stale replacement")])
"#,
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );

    broker
        .refresh_documents(
            FAKE_LANGUAGE,
            vec![
                fake_document(FAKE_LANGUAGE, "src/a.fake", "initial a"),
                fake_document(FAKE_LANGUAGE, "src/b.fake", "initial b"),
            ],
            FAKE_LSP_TIMEOUT,
        )
        .await
        .expect("both initial versionless publications should be accepted");
    assert_eq!(broker.snapshot().diagnostics.len(), 2);

    let initial = broker.snapshot();
    assert!(matches!(
        initial.coverage[FAKE_LANGUAGE].documents["src/b.fake"],
        lsp::client::DiagnosticDocumentCoverage::Published {
            version_verified: false,
            ..
        }
    ));
    let retained = initial
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.file == "src/b.fake")
        .unwrap();

    broker
        .refresh_documents(
            FAKE_LANGUAGE,
            vec![
                fake_document(FAKE_LANGUAGE, "src/a.fake", "updated a"),
                fake_document(FAKE_LANGUAGE, "src/b.fake", "updated b"),
            ],
            FAKE_LSP_TIMEOUT,
        )
        .await
        .expect("an explicit clean publication makes the partial refresh useful");

    let partial = broker.snapshot();
    assert_eq!(partial.diagnostics, vec![retained.clone()]);
    assert_eq!(
        partial.coverage[FAKE_LANGUAGE].documents["src/b.fake"],
        lsp::client::DiagnosticDocumentCoverage::NoPublication,
    );
    assert!(matches!(
        partial.coverage[FAKE_LANGUAGE].documents["src/a.fake"],
        lsp::client::DiagnosticDocumentCoverage::Published {
            version_verified: true,
            ..
        }
    ));
    let published = serde_json::to_value(&partial).unwrap();
    assert_eq!(
        published["coverage"][FAKE_LANGUAGE]["documents"]["src/b.fake"]["state"],
        "no_publication"
    );
    assert_eq!(
        published["coverage"][FAKE_LANGUAGE]["documents"]["src/a.fake"]["version_verified"],
        true
    );

    broker
        .refresh_documents(
            FAKE_LANGUAGE,
            vec![
                fake_document(FAKE_LANGUAGE, "src/a.fake", "updated a again"),
                fake_document(FAKE_LANGUAGE, "src/b.fake", "stale b"),
            ],
            FAKE_LSP_TIMEOUT,
        )
        .await
        .expect("the current clean publication makes a stale sibling incomplete");

    let stale = broker.snapshot();
    assert_eq!(
        stale.coverage[FAKE_LANGUAGE].documents["src/b.fake"],
        lsp::client::DiagnosticDocumentCoverage::StalePublication,
    );
    let published = serde_json::to_value(&stale).unwrap();
    assert_eq!(
        published["coverage"][FAKE_LANGUAGE]["documents"]["src/b.fake"]["state"],
        "stale_publication"
    );
    let diagnostics = stale.diagnostics;
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].file, "src/b.fake");
    assert_eq!(diagnostics[0].message, "initial finding");

    broker
        .refresh_documents(
            FAKE_LANGUAGE,
            vec![fake_document(
                FAKE_LANGUAGE,
                "src/a.fake",
                "single document refresh",
            )],
            FAKE_LSP_TIMEOUT,
        )
        .await
        .unwrap();
    let narrowed = broker.snapshot();
    assert_eq!(narrowed.coverage[FAKE_LANGUAGE].documents.len(), 1);
    assert!(
        !narrowed.coverage[FAKE_LANGUAGE]
            .documents
            .contains_key("src/b.fake")
    );
    assert_eq!(narrowed.diagnostics, vec![retained.clone()]);
}

#[tokio::test]
async fn queued_closed_overlays_preserve_the_analyzer_and_sibling_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("queued_overlay_lsp.py");
    let events_path = temp.path().join("synchronized.jsonl");
    let publication = format!(
        r#"        text = message["params"]["textDocument"].get("text")
        if text is None:
            text = message["params"]["contentChanges"][0]["text"]
        with open({events:?}, "a", encoding="utf-8") as events:
            events.write(json.dumps({{"uri": uri, "text": text}}) + "\n")
{FAKE_DIAGNOSTIC_PUBLISH}"#,
        events = events_path.display().to_string(),
    );
    std::fs::write(
        &script_path,
        fake_lsp_script_with_preamble("", &publication),
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "saved disk")],
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap();
    let retained = broker.snapshot().diagnostics[0].clone();
    let root_uri = url::Url::from_directory_path(temp.path())
        .unwrap()
        .to_string();
    let uri = format!("{root_uri}{FAKE_PATH}");
    let authority = broker
        .semantic_authority_if_available(
            FAKE_LANGUAGE,
            temp.path().to_path_buf(),
            root_uri.clone(),
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap()
        .unwrap();
    let attempt = authority.analyzer_readiness().attempt();
    let mut overlays = tracedecay_lsp::OverlayStore::default();
    for include_sibling in [false, true] {
        let overlay = overlays
            .open(
                &AdmittedRoot::new(root_uri.clone()),
                uri.clone(),
                "fake",
                1,
                "queued closed overlay",
            )
            .unwrap();
        let mut document = fake_document(FAKE_LANGUAGE, FAKE_PATH, &overlay.text);
        document.snapshot = Some(tracedecay_lsp::LspSemanticDocumentSnapshot::from_overlay(
            &overlay,
        ));
        let mut documents = vec![document];
        if include_sibling {
            documents.push(fake_document(
                FAKE_LANGUAGE,
                "src/sibling.fake",
                "live sibling",
            ));
        }
        let prepared = broker
            .prepare_refresh(FAKE_LANGUAGE, documents)
            .await
            .unwrap()
            .unwrap();
        overlays.close(&uri).unwrap();
        let completed = prepared
            .collect_diagnostics_with_timeouts(recovery_fake_lsp_timeouts())
            .await;
        assert!(
            completed.is_ok(),
            "closing an overlay is not an analyzer failure"
        );
        let refresh = completed.diagnostic_refresh().unwrap();
        assert_eq!(
            refresh.coverage.documents[FAKE_PATH],
            lsp::client::DiagnosticDocumentCoverage::StalePublication
        );
        assert!(
            refresh
                .diagnostics
                .iter()
                .all(|finding| finding.file != FAKE_PATH)
        );
        if include_sibling {
            assert!(matches!(
                refresh.coverage.documents["src/sibling.fake"],
                lsp::client::DiagnosticDocumentCoverage::Published { .. }
            ));
            assert_eq!(refresh.diagnostics.len(), 1);
        }
        broker.finish_refresh(completed).unwrap();
        let snapshot = broker.snapshot();
        assert_eq!(
            snapshot
                .diagnostics
                .iter()
                .filter(|finding| finding.file == FAKE_PATH)
                .collect::<Vec<_>>(),
            vec![&retained]
        );
        assert_engine_state(&snapshot, FAKE_LANGUAGE, lsp::broker::EngineState::Ready);
        let readiness = authority.analyzer_readiness();
        assert_eq!(readiness.state(), AnalyzerState::Ready);
        assert_eq!(readiness.attempt(), attempt);
        assert_eq!(readiness.restart_attempts(), 0);
        assert_eq!(readiness.last_failure(), None);
    }
    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(FAKE_LANGUAGE, FAKE_PATH, "saved update")],
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap();
    assert_eq!(authority.analyzer_readiness().attempt(), attempt);
    let events = std::fs::read_to_string(events_path).unwrap();
    let texts = events
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["text"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        texts,
        ["saved disk", "live sibling", "saved update"],
        "queued closed overlay bytes must never reach the analyzer"
    );
}

#[tokio::test]
async fn warm_diagnostic_coverage_requires_current_evidence_after_a_stale_publication() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("warm_coverage_lsp.py");
    let control = FakeLspPhaseControl::bind().await;
    let server = r#"
import json
import sys

documents = {}
opened_a = False

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
    return json.loads(sys.stdin.buffer.read(int(headers["content-length"])))

def send(payload):
    body = json.dumps(payload).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: " + str(len(body)).encode("ascii") + b"\r\n\r\n" + body)
    sys.stdout.buffer.flush()

def publish(uri, version, message=None):
    diagnostics = [] if message is None else [{
        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}},
        "severity": 1,
        "message": message,
        "source": "fake-ls"
    }]
    send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
          "params": {"uri": uri, "version": version, "diagnostics": diagnostics}})

def synchronize_overlay_before_publication(text):
    if text == "closed overlay finding":
        tracedecay_maybe_phase("overlay-synchronized")

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {}}})
    elif method == "textDocument/didOpen":
        document = message["params"]["textDocument"]
        uri = document["uri"]
        reopened_alias = uri.endswith("a.fake") and opened_a
        if uri.endswith("a.fake"):
            opened_a = True
        documents[uri] = document["version"]
        synchronize_overlay_before_publication(document["text"])
        message = document["text"] if reopened_alias else ("retained finding" if uri.endswith("a.fake") else None)
        publish(uri, document["version"], message)
    elif method == "textDocument/didClose":
        documents.pop(message["params"]["textDocument"]["uri"], None)
    elif method == "textDocument/didChange":
        document = message["params"]["textDocument"]
        uri = document["uri"]
        text = message["params"]["contentChanges"][0]["text"]
        documents[uri] = document["version"]
        if uri.endswith("a.fake"):
            synchronize_overlay_before_publication(text)
            publish(uri, document["version"], text)
            continue
        target = next(key for key in documents if key.endswith("a.fake"))
        if text == "stale":
            publish(target, documents[target] - 1, "stale replacement")
        elif text == "repair":
            publish(target, documents[target], "current finding")
        elif text == "current then stale":
            publish(target, documents[target], "new current finding")
            publish(target, documents[target] - 1, "stale replacement")
        publish(uri, document["version"])
"#;
    std::fs::write(
        &script_path,
        format!(
            "{}\n{server}",
            control.script_preamble(&["overlay-synchronized"])
        ),
    )
    .unwrap();
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(
        temp.path(),
        vec![fake_python_adapter(FAKE_LANGUAGE, "fake", &script_path)],
    );
    let mut retained = Vec::new();
    for (trigger, expected_coverage, expected_message) in [
        ("initial", "published", "retained finding"),
        ("stale", "stale_publication", "retained finding"),
        ("silent", "no_publication", "retained finding"),
        ("repair", "published", "current finding"),
        ("current then stale", "published", "new current finding"),
    ] {
        broker
            .refresh_documents_with_timeouts(
                FAKE_LANGUAGE,
                vec![
                    fake_document(FAKE_LANGUAGE, "src/a.fake", "unchanged"),
                    fake_document(FAKE_LANGUAGE, "src/b.fake", trigger),
                ],
                recovery_fake_lsp_timeouts(),
            )
            .await
            .expect("a current sibling makes each refresh useful");
        let snapshot = broker.snapshot();
        let coverage =
            serde_json::to_value(&snapshot.coverage[FAKE_LANGUAGE].documents["src/a.fake"])
                .unwrap();
        assert_eq!(coverage["state"], expected_coverage, "trigger: {trigger}");
        assert_eq!(snapshot.diagnostics.len(), 1, "trigger: {trigger}");
        assert_eq!(snapshot.diagnostics[0].file, "src/a.fake");
        if matches!(trigger, "stale" | "silent") {
            assert_eq!(
                snapshot.diagnostics, retained,
                "unverified findings must remain byte-for-byte unchanged"
            );
        } else {
            retained = snapshot.diagnostics.clone();
        }
        assert_eq!(
            snapshot.diagnostics[0].message, expected_message,
            "trigger: {trigger}"
        );
    }

    let root_uri = url::Url::from_directory_path(temp.path())
        .unwrap()
        .to_string();
    let uri = format!("{root_uri}src/a.fake");
    let mut overlays = tracedecay_lsp::OverlayStore::default();
    let overlay = overlays
        .open(
            &AdmittedRoot::new(root_uri.clone()),
            uri.clone(),
            "fake",
            42,
            "closed overlay finding",
        )
        .unwrap();
    let mut document = fake_document(FAKE_LANGUAGE, "src/a.fake", &overlay.text);
    document.snapshot = Some(tracedecay_lsp::LspSemanticDocumentSnapshot::from_overlay(
        &overlay,
    ));
    let prepared = broker
        .prepare_refresh(FAKE_LANGUAGE, vec![document])
        .await
        .unwrap()
        .expect("overlay refresh");
    let collecting = tokio::spawn(async move {
        prepared
            .collect_diagnostics_with_timeouts(phase_gated_fake_lsp_timeouts())
            .await
    });
    let synchronized = control.wait_for("overlay-synchronized").await;
    overlays.close(&uri).unwrap();
    synchronized.release().await;
    let completed = collecting.await.unwrap();
    let refresh = completed
        .diagnostic_refresh()
        .expect("closed overlay has partial coverage");
    assert!(
        refresh.diagnostics.is_empty(),
        "closed overlay values cannot append to retained findings"
    );
    assert_eq!(
        refresh.coverage.documents["src/a.fake"],
        lsp::client::DiagnosticDocumentCoverage::StalePublication
    );
    broker.finish_refresh(completed).unwrap();
    assert_eq!(
        broker.snapshot().diagnostics,
        retained,
        "the broker retains exactly its old findings when the new overlay closes"
    );

    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            vec![fake_document(
                FAKE_LANGUAGE,
                "src/a.fake",
                "saved replacement",
            )],
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap();
    let saved = broker.snapshot();
    assert_eq!(saved.diagnostics.len(), 1);
    assert_eq!(saved.diagnostics[0].file, "src/a.fake");
    assert_eq!(saved.diagnostics[0].message, "saved replacement");

    let mut previous_findings = saved.diagnostics.clone();
    for close_previous_lifetime in [false, true] {
        if close_previous_lifetime {
            overlays.close(&uri).unwrap();
        }
        let reopened = overlays
            .open(
                &AdmittedRoot::new(root_uri.clone()),
                uri.clone(),
                "fake",
                1,
                "saved replacement",
            )
            .unwrap();
        let mut document = fake_document(FAKE_LANGUAGE, "src/a.fake", &reopened.text);
        document.snapshot = Some(tracedecay_lsp::LspSemanticDocumentSnapshot::from_overlay(
            &reopened,
        ));
        broker
            .refresh_documents_with_timeouts(
                FAKE_LANGUAGE,
                vec![document],
                recovery_fake_lsp_timeouts(),
            )
            .await
            .unwrap();
        let unchanged = broker.snapshot();
        if close_previous_lifetime {
            assert_eq!(
                unchanged.diagnostics, previous_findings,
                "a new lifetime with the retained presentation URI reuses the unchanged wire document"
            );
        } else {
            let mut normalized = unchanged.diagnostics.clone();
            for (finding, saved_finding) in normalized.iter_mut().zip(&saved.diagnostics) {
                finding.updated_at = saved_finding.updated_at;
            }
            assert_eq!(
                normalized, saved.diagnostics,
                "a presentation-URI reopen republishes the same finding without duplicating it"
            );
        }
        assert_eq!(
            unchanged.coverage[FAKE_LANGUAGE].documents["src/a.fake"],
            lsp::client::DiagnosticDocumentCoverage::Published {
                version_verified: true,
                content_digest: tracedecay_domain::ContentDigest::of_bytes(b"saved replacement"),
            }
        );
        previous_findings = unchanged.diagnostics;
    }
}

#[tokio::test]
async fn successful_root_findings_survive_a_failed_sibling_root() {
    let temp = tempfile::tempdir().unwrap();
    let member = temp.path().join("member");
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::create_dir_all(member.join("src")).unwrap();
    std::fs::write(temp.path().join("workspace.marker"), "").unwrap();
    std::fs::write(member.join("workspace.marker"), "").unwrap();
    std::fs::write(temp.path().join("src/a.fake"), "initial root").unwrap();
    std::fs::write(member.join("src/b.fake"), "initial nested").unwrap();
    let script_path = temp.path().join("rooted_diagnostics_lsp.py");
    std::fs::write(
        &script_path,
        r#"
import json
import os
import sys

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

def publish(uri, message):
    send({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": uri,
            "diagnostics": [{
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 1}
                },
                "severity": 1,
                "message": message,
                "source": "fake-ls"
            }]
        }
    })

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"capabilities": {}}})
    elif method == "textDocument/didOpen":
        document = message["params"]["textDocument"]
        publish(document["uri"], "initial " + document["text"])
    elif method == "textDocument/didChange":
        document = message["params"]["textDocument"]
        text = message["params"]["contentChanges"][0]["text"]
        if os.path.basename(os.getcwd()) == "member" and text == "fail nested":
            sys.exit(17)
        publish(document["uri"], "updated " + text)
"#,
    )
    .unwrap();
    let adapter = fake_adapter_with_root_markers(
        FAKE_LANGUAGE,
        "fake",
        python_command(),
        vec![script_path.display().to_string()],
        vec!["workspace.marker".to_owned()],
    );
    let mut broker = lsp::broker::DiagnosticBroker::new_for_test(temp.path(), vec![adapter]);
    let documents = |root_text: &str, nested_text: &str| {
        vec![
            fake_document(FAKE_LANGUAGE, "src/a.fake", root_text),
            fake_document(FAKE_LANGUAGE, "member/src/b.fake", nested_text),
        ]
    };

    broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            documents("initial root", "initial nested"),
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .expect("both workspace roots should initially publish");
    assert_eq!(broker.snapshot().diagnostics.len(), 2);

    let error = broker
        .refresh_documents_with_timeouts(
            FAKE_LANGUAGE,
            documents("updated root", "fail nested"),
            loaded_runner_fake_lsp_timeouts(),
        )
        .await
        .expect_err("a failed sibling root must remain visible to the caller");
    assert!(error.to_string().contains("workspace root"));
    let partial = broker.snapshot();
    let coverage = &partial.coverage[FAKE_LANGUAGE];
    assert_eq!(
        coverage.documents["member/src/b.fake"],
        lsp::client::DiagnosticDocumentCoverage::RootFailed,
    );
    assert_eq!(
        coverage.roots[&temp.path().canonicalize().unwrap()],
        lsp::client::DiagnosticRootCoverage::Completed,
    );
    assert_eq!(
        coverage.roots[&member.canonicalize().unwrap()],
        lsp::client::DiagnosticRootCoverage::Failed,
    );
    let published = serde_json::to_value(&partial).unwrap();
    assert_eq!(
        published["coverage"][FAKE_LANGUAGE]["documents"]["member/src/b.fake"]["state"],
        "root_failed",
    );
    let diagnostics = partial.diagnostics;
    assert_eq!(diagnostics.len(), 2);
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.file == "src/a.fake" && diagnostic.message == "updated updated root"
    }));
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.file == "member/src/b.fake" && diagnostic.message == "initial initial nested"
    }));
}

#[tokio::test]
async fn semantic_cancellation_reaches_and_releases_the_registered_operation() {
    let temp = tempfile::tempdir().unwrap();
    let script_path = temp.path().join("cancelled_lsp.py");
    std::fs::write(&script_path, fake_lsp_script_that_never_initializes()).unwrap();
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
            recovery_fake_lsp_timeouts(),
        )
        .await
        .unwrap()
        .expect("fake analyzer is executable");
    let root = AdmittedRoot::new(root_uri.clone());
    let request_id = LspRequestId::Number(19);
    let operation = authority.start(
        root.clone(),
        request_id.clone(),
        tracedecay_lsp::lsp_semantic_request(&SemanticRequest::DocumentSymbols {
            document_uri: format!("{root_uri}{FAKE_PATH}"),
        })
        .unwrap(),
    );

    assert!(authority.cancel_request(&root, &request_id));
    assert!(matches!(
        operation.await,
        tracedecay_lsp::LspSemanticOperationOutcome::Partial { ref coverage, .. }
            if coverage == "semantic-cancelled"
    ));
    assert!(!authority.cancel_request(&root, &request_id));
    assert!(!authority.cancel_request(&root, &LspRequestId::Number(20)));
}
