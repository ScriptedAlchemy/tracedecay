use super::*;
use crate::LspSemanticOperationOutcome;
use crate::analyzer::activity::{
    project_root_canonicalization_count, reset_project_root_canonicalization_count,
};
use crate::analyzer::adapters::DiagnosticMode;
use crate::analyzer::client::LspSemanticRequestError;
use crate::analyzer::error::AnalyzerRuntimeError as TraceDecayError;

fn assert_safe_partial_detail(
    outcome: LspSemanticOperationOutcome,
    expected_coverage: &str,
    expected_detail: &str,
) {
    let LspSemanticOperationOutcome::Partial {
        coverage, detail, ..
    } = outcome
    else {
        panic!("expected partial semantic outcome");
    };
    assert_eq!(coverage, expected_coverage);
    assert_eq!(detail, Some(expected_detail));
    for forbidden in [
        "bearer-secret",
        "YWxpY2U6c2VjcmV0",
        "alice:hunter2",
        "bob:password",
        "/home/alice",
        r"C:\Users\alice",
        "密碼",
        "🔐",
    ] {
        assert!(
            !detail
                .expect("typed analyzer failure detail")
                .contains(forbidden),
            "caller detail leaked {forbidden}"
        );
    }
}

#[test]
fn analyzer_failure_details_never_copy_raw_errors() {
    let sensitive = concat!(
        "stderr first line\n",
        "Authorization: Bearer bearer-secret\n",
        "Authorization: Basic YWxpY2U6c2VjcmV0\n",
        "https://alice:hunter2@example.test/private\n",
        "file://bob:password@localhost/home/bob/private.rs\n",
        "/home/alice/.ssh/id_rsa\n",
        r"C:\Users\alice\AppData\secret.txt",
        "\nUTF-8: 密碼 🔐"
    );

    assert_safe_partial_detail(
        analyzer_start_failure(&TraceDecayError::Config {
            message: sensitive.to_owned(),
        }),
        "analyzer-start-failed",
        LspSemanticOperationOutcome::ANALYZER_START_FAILED_DETAIL,
    );
    assert_safe_partial_detail(
        semantic_operation_outcome(Err(LspSemanticRequestError::Remote {
            code: Some(-32603),
            message: sensitive.to_owned(),
        })),
        "analyzer-remote-error",
        LspSemanticOperationOutcome::ANALYZER_REMOTE_ERROR_DETAIL,
    );
    assert_safe_partial_detail(
        semantic_operation_outcome(Err(LspSemanticRequestError::Transport {
            class: sensitive.to_owned(),
        })),
        "analyzer-transport-failed",
        LspSemanticOperationOutcome::ANALYZER_TRANSPORT_FAILED_DETAIL,
    );
    assert_safe_partial_detail(
        semantic_operation_outcome(Err(LspSemanticRequestError::InvalidResponse {
            class: sensitive.to_owned(),
        })),
        "analyzer-invalid-response",
        LspSemanticOperationOutcome::ANALYZER_INVALID_RESPONSE_DETAIL,
    );
    assert_safe_partial_detail(
        semantic_operation_outcome(Err(LspSemanticRequestError::TimedOut)),
        "analyzer-timeout",
        LspSemanticOperationOutcome::ANALYZER_TIMEOUT_DETAIL,
    );
    assert_safe_partial_detail(
        semantic_operation_outcome(Err(LspSemanticRequestError::Cancelled)),
        "analyzer-cancelled",
        LspSemanticOperationOutcome::ANALYZER_CANCELLED_DETAIL,
    );
}

#[test]
fn semantic_remote_method_missing_remains_unavailable() {
    assert_eq!(
        semantic_operation_outcome(Err(LspSemanticRequestError::Remote {
            code: Some(-32601),
            message: "method not found: stale Bearer secret /private/path?!".to_owned(),
        })),
        LspSemanticOperationOutcome::Unavailable
    );
}

fn adapter(
    language: &str,
    command: impl Into<String>,
    extension: &str,
    root_marker: &str,
) -> LspAdapterDefinition {
    LspAdapterDefinition {
        language: language.to_owned(),
        language_id: language.to_owned(),
        command: command.into(),
        args: Vec::new(),
        extensions: vec![extension.to_owned()],
        root_markers: vec![root_marker.to_owned()],
        install_options: Vec::new(),
        diagnostics: DiagnosticMode::Push,
    }
}

#[test]
fn absent_analyzer_keeps_an_admitted_graph_fallback_provider() {
    let project = tempfile::tempdir().expect("project");
    std::fs::write(project.path().join("Cargo.toml"), "").expect("rust root marker");
    let missing = project.path().join("missing-rust-analyzer");
    let mut broker = DiagnosticBroker::new(
        project.path(),
        vec![adapter(
            "rust",
            missing.to_string_lossy(),
            "rs",
            "Cargo.toml",
        )],
        CodeDiagnosticsSettings::default(),
    );

    assert_eq!(
        broker.admitted_providers_for_files(&["src/lib.rs".to_owned()]),
        vec![AdmittedLspProvider {
            language: "rust".to_owned(),
            command: missing.to_string_lossy().into_owned(),
            analyzer_available: false,
        }]
    );
    assert!(
        broker
            .semantic_authority_if_available(
                "rust",
                project.path().to_path_buf(),
                url::Url::from_directory_path(project.path())
                    .expect("project root URI")
                    .to_string(),
                LspRefreshTimeouts::from_diagnostics_quiet_window(
                    std::time::Duration::from_millis(10),
                ),
            )
            .expect("configured adapter")
            .is_none()
    );
    assert!(
        broker
            .mounted_providers_for_files(&["src/lib.rs".to_owned()])
            .is_empty()
    );
}

#[cfg(unix)]
mod rustup_proxy {
    use super::*;
    use crate::analyzer::launch::fake_rustup;
    use crate::analyzer::launch::{NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE, RUSTUP_AUTO_INSTALL_ENV};

    fn rust_project() -> tempfile::TempDir {
        let project = tempfile::tempdir().expect("project");
        std::fs::write(project.path().join("Cargo.toml"), "").expect("rust root marker");
        std::fs::create_dir(project.path().join("src")).expect("src");
        std::fs::write(project.path().join("src/lib.rs"), "pub fn lib() {}").expect("lib.rs");
        project
    }

    fn rust_document() -> LspDocument {
        LspDocument {
            language: "rust".to_owned(),
            language_id: "rust".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            text: "pub fn lib() {}".to_owned(),
        }
    }

    /// The dogfood defect: the configured `rust-analyzer` is a rustup proxy
    /// and the project's active toolchain lacks the component. The broker
    /// must report the typed state and never run the proxy, which is what
    /// would download the toolchain.
    #[test]
    fn missing_toolchain_component_is_typed_unavailable_without_an_install() {
        let rustup = fake_rustup::install(None);
        let project = rust_project();
        let command = rustup.path().join("rust-analyzer");
        let mut broker = DiagnosticBroker::new_for_test(
            project.path(),
            vec![adapter(
                "rust",
                command.to_string_lossy(),
                "rs",
                "Cargo.toml",
            )],
        );
        let files = vec!["src/lib.rs".to_owned()];

        assert_eq!(
            broker.admitted_providers_for_files(&files),
            vec![AdmittedLspProvider {
                language: "rust".to_owned(),
                command: command.to_string_lossy().into_owned(),
                analyzer_available: false,
            }]
        );

        let error = broker
            .prepare_refresh("rust", vec![rust_document()])
            .err()
            .expect("a missing component must refuse the refresh");
        let TraceDecayError::Config { message } = &error else {
            panic!("a missing component is a typed configuration refusal, got {error:?}");
        };
        assert!(
            message.starts_with(NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE),
            "{message}"
        );
        assert!(
            !message.contains('\n') && !message.contains("syncing channel"),
            "{message}"
        );

        let engine = broker.snapshot().engines.remove(0);
        assert_eq!(engine.state, EngineState::Unavailable);
        let last_error = engine.last_error.expect("typed engine error");
        assert!(last_error.starts_with(NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE));
        assert!(!last_error.contains('\n'));
        assert!(!last_error.contains("stderr"));

        assert!(
            broker
                .semantic_authority_if_available(
                    "rust",
                    project.path().to_path_buf(),
                    url::Url::from_directory_path(project.path())
                        .expect("project root URI")
                        .to_string(),
                    LspRefreshTimeouts::from_diagnostics_quiet_window(
                        std::time::Duration::from_millis(10),
                    ),
                )
                .expect("configured adapter")
                .is_none()
        );

        let invocations = fake_rustup::invocations(rustup.path());
        let probes = invocations
            .lines()
            .filter(|line| line.ends_with(" which rust-analyzer"))
            .count();
        let proxy_runs = invocations
            .lines()
            .filter(|line| line.contains("rust-analyzer") && !line.contains(" which "))
            .count();
        assert!(probes >= 1, "{invocations}");
        assert_eq!(
            proxy_runs, 0,
            "the proxy itself must never run: {invocations}"
        );
        assert!(
            !invocations.contains("AUTO_INSTALL=unset"),
            "every probe must carry {RUSTUP_AUTO_INSTALL_ENV}=0: {invocations}"
        );
        assert!(broker.clients.is_empty());
    }

    #[test]
    fn installed_component_launches_the_toolchain_binary_not_the_proxy() {
        let real = tempfile::tempdir().expect("real analyzer");
        let real_binary = real.path().join("rust-analyzer");
        std::fs::write(&real_binary, "").expect("real analyzer binary");
        let rustup = fake_rustup::install(Some(&real_binary));
        let project = rust_project();
        let command = rustup.path().join("rust-analyzer");
        let mut broker = DiagnosticBroker::new_for_test(
            project.path(),
            vec![adapter(
                "rust",
                command.to_string_lossy(),
                "rs",
                "Cargo.toml",
            )],
        );

        assert!(
            broker
                .admitted_providers_for_files(&["src/lib.rs".to_owned()])
                .iter()
                .all(|provider| provider.analyzer_available)
        );
        let prepared = broker
            .prepare_refresh("rust", vec![rust_document()])
            .expect("installed component prepares")
            .expect("active language prepares");
        assert_eq!(prepared.launch().program, real_binary);
        assert_eq!(
            prepared.launch().env,
            vec![(RUSTUP_AUTO_INSTALL_ENV.to_owned(), "0".to_owned())]
        );
        assert_eq!(broker.snapshot().engines[0].state, EngineState::Refreshing);
    }
}

#[test]
fn refresh_rejects_a_removed_project_root_after_one_canonicalization() {
    let temp = tempfile::tempdir().expect("temporary parent");
    let project = temp.path().join("removed-project");
    std::fs::create_dir(&project).expect("project directory");
    let command = std::env::current_exe().expect("current executable");
    let mut broker = DiagnosticBroker::new_for_test(
        &project,
        vec![adapter(
            "rust",
            command.to_string_lossy(),
            "rs",
            "Cargo.toml",
        )],
    );
    std::fs::remove_dir(&project).expect("remove project directory");
    reset_project_root_canonicalization_count();

    let Err(error) = broker.prepare_refresh(
        "rust",
        vec![LspDocument {
            language: "rust".to_owned(),
            language_id: "rust".to_owned(),
            relative_path: "src/lib.rs".to_owned(),
            text: "fn removed_root() {}".to_owned(),
        }],
    ) else {
        panic!("removed project root must fail closed");
    };

    assert!(
        error
            .to_string()
            .contains("failed to resolve admitted project root")
    );
    assert_eq!(project_root_canonicalization_count(), 1);
    assert!(broker.clients.is_empty());
}

#[test]
fn refresh_rejects_root_batch_queue_saturation_before_starting_analyzers() {
    let project = tempfile::tempdir().expect("project");
    let command = project.path().join("analyzer");
    std::fs::write(&command, "").expect("analyzer command");
    let mut documents = Vec::with_capacity(MAX_ANALYZER_QUEUED_ROOT_BATCHES + 1);
    for index in 0..=MAX_ANALYZER_QUEUED_ROOT_BATCHES {
        let package = project.path().join(format!("package-{index}"));
        std::fs::create_dir_all(package.join("src")).expect("package source directory");
        std::fs::write(package.join("marker"), "").expect("package root marker");
        documents.push(LspDocument {
            language: "rust".to_owned(),
            language_id: "rust".to_owned(),
            relative_path: format!("package-{index}/src/lib.rs"),
            text: "fn package() {}".to_owned(),
        });
    }
    let mut broker = DiagnosticBroker::new_for_test(
        project.path(),
        vec![adapter("rust", command.to_string_lossy(), "rs", "marker")],
    );

    let Err(error) = broker.prepare_refresh("rust", documents) else {
        panic!("queue saturation must reject before analyzer startup");
    };

    assert!(error.to_string().contains("analyzer root queue saturated"));
    assert_eq!(broker.snapshot().engines[0].state, EngineState::Unavailable);
}

#[test]
fn superseded_refresh_cannot_supply_a_snapshot_to_its_caller() {
    let mut broker = DiagnosticBroker::new_for_test("/project", Vec::new());
    broker.refresh_epochs.insert("rust".to_owned(), 2);
    let completed = |epoch| CompletedRefresh {
        language: "rust".to_owned(),
        command: "rust-analyzer".to_owned(),
        epoch,
        result: Ok(Vec::new()),
    };

    assert!(matches!(
        broker.finish_refresh_snapshot(completed(1)),
        Ok(RefreshCommitOutcome::Superseded)
    ));
    assert!(matches!(
        broker.finish_refresh_snapshot(completed(3)),
        Ok(RefreshCommitOutcome::Superseded)
    ));
}
