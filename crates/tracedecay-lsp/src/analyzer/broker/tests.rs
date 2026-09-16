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

    /// Project-open admits providers without running a refresh. The proxy is
    /// on PATH, so the default engine state would read `Available`; the
    /// admission probe that finds the component missing must record the typed
    /// `Unavailable` state itself, or the snapshot stays wrong until a refresh
    /// that may never come.
    #[test]
    fn failed_admission_probe_records_the_typed_unavailable_state() {
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
        assert_eq!(
            broker.snapshot().engines[0].state,
            EngineState::Available,
            "before any probe the proxy on PATH reads as available"
        );

        let admitted = broker.admitted_providers_for_files(&["src/lib.rs".to_owned()]);

        assert_eq!(
            admitted,
            vec![AdmittedLspProvider {
                language: "rust".to_owned(),
                command: command.to_string_lossy().into_owned(),
                analyzer_available: false,
            }]
        );
        let engine = broker.snapshot().engines.remove(0);
        assert_eq!(engine.state, EngineState::Unavailable);
        let last_error = engine.last_error.expect("the failed probe is recorded");
        assert!(
            last_error.starts_with(NOT_INSTALLED_FOR_TOOLCHAIN_MESSAGE),
            "{last_error}"
        );
        assert!(!last_error.contains('\n') && !last_error.contains("syncing channel"));
        let invocations = fake_rustup::invocations(rustup.path());
        assert!(
            invocations.contains(" which rust-analyzer"),
            "{invocations}"
        );
        assert!(!invocations.contains("AUTO_INSTALL=unset"), "{invocations}");
        assert_eq!(
            invocations
                .lines()
                .filter(|line| line.contains("rust-analyzer") && !line.contains(" which "))
                .count(),
            0,
            "the proxy itself never runs: {invocations}"
        );
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
        let launches = prepared.batch_launches();
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].1.program, real_binary);
        assert_eq!(
            launches[0].1.env,
            vec![(RUSTUP_AUTO_INSTALL_ENV.to_owned(), "0".to_owned())]
        );
        assert_eq!(broker.snapshot().engines[0].state, EngineState::Refreshing);
    }

    /// Nested analyzer roots may pin different toolchains. rustup resolves the
    /// override from the directory it runs in, so the launch must be resolved
    /// from each batch's own workspace root — never the project root's
    /// answer reused for every batch — and retained per root.
    #[test]
    fn nested_root_with_its_own_toolchain_override_launches_its_own_binary() {
        let rustup = fake_rustup::install_per_root();
        let project = rust_project();
        let member = project.path().join("member");
        std::fs::create_dir_all(member.join("src")).expect("member src");
        std::fs::write(member.join("Cargo.toml"), "").expect("member root marker");
        std::fs::write(member.join("src/lib.rs"), "pub fn member() {}").expect("member lib.rs");
        let binaries = tempfile::tempdir().expect("toolchain binaries");
        let root_binary = binaries.path().join("stable/rust-analyzer");
        let member_binary = binaries.path().join("nightly/rust-analyzer");
        for binary in [&root_binary, &member_binary] {
            std::fs::create_dir_all(binary.parent().expect("toolchain dir")).expect("toolchain");
            std::fs::write(binary, "").expect("toolchain analyzer");
        }
        std::fs::write(
            project.path().join(fake_rustup::PER_ROOT_ANALYZER_FILE),
            format!("{}\n", root_binary.display()),
        )
        .expect("root override");
        std::fs::write(
            member.join(fake_rustup::PER_ROOT_ANALYZER_FILE),
            format!("{}\n", member_binary.display()),
        )
        .expect("member override");
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
        let documents = || {
            vec![
                rust_document(),
                LspDocument {
                    language: "rust".to_owned(),
                    language_id: "rust".to_owned(),
                    relative_path: "member/src/lib.rs".to_owned(),
                    text: "pub fn member() {}".to_owned(),
                },
            ]
        };
        let canonical_root = project.path().canonicalize().expect("canonical project");
        let canonical_member = member.canonicalize().expect("canonical member");

        let prepared = broker
            .prepare_refresh("rust", documents())
            .expect("both toolchains have the component")
            .expect("active language prepares");

        let launches = prepared.batch_launches();
        assert_eq!(
            launches
                .iter()
                .map(|(root, launch)| (root.to_path_buf(), launch.program.clone()))
                .collect::<Vec<_>>(),
            vec![
                (canonical_root.clone(), root_binary.clone()),
                (canonical_member.clone(), member_binary.clone()),
            ],
            "each batch launches the binary its own root resolves to"
        );
        assert!(
            launches.iter().all(|(_, launch)| {
                launch.env == vec![(RUSTUP_AUTO_INSTALL_ENV.to_owned(), "0".to_owned())]
            }),
            "every batch carries the no-install environment"
        );
        let invocations = fake_rustup::invocations(rustup.path());
        for root in [&canonical_root, &canonical_member] {
            assert!(
                invocations.contains(&format!("PWD={}", root.display())),
                "rustup which must run from {}: {invocations}",
                root.display()
            );
        }
        assert!(!invocations.contains("AUTO_INSTALL=unset"), "{invocations}");
        let probes = |log: &str| log.lines().filter(|line| line.contains(" which ")).count();
        assert_eq!(probes(&invocations), 2, "one probe per root: {invocations}");
        drop(prepared);

        broker
            .prepare_refresh("rust", documents())
            .expect("retained launches prepare")
            .expect("active language prepares");
        assert_eq!(
            probes(&fake_rustup::invocations(rustup.path())),
            2,
            "a resolved root is retained, not probed again"
        );
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
