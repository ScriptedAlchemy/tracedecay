use std::sync::Arc;

use tokio::sync::Mutex;
use tracedecay_lsp::analyzer::adapters::{DiagnosticMode, LspAdapterDefinition};
use tracedecay_lsp::analyzer::broker::DiagnosticBroker;
use tracedecay_lsp::analyzer::settings::CodeDiagnosticsSettings;

use crate::application::dashboard_diagnostics::{
    DashboardDiagnosticsAuthorityV1, DashboardDiagnosticsErrorV1,
};
use crate::tracedecay::TraceDecay;

#[tokio::test]
async fn failed_refresh_is_an_error_and_never_claims_documents_opened() {
    let _pin = crate::config::PinnedUserDataDir::new();
    let project = tempfile::tempdir().expect("project");
    std::fs::write(project.path().join("fixture.rs"), "fn fixture() {}\n").expect("fixture source");
    let (graph, _runtime) = TraceDecay::init_test_fixture_with_registered_runtime(
        project.path(),
        "project.dashboard-diagnostics-refresh",
    )
    .await
    .expect("test runtime");
    graph.sync().await.expect("index fixture source");
    let adapter = LspAdapterDefinition {
        language: "fake".to_owned(),
        language_id: "fake".to_owned(),
        command: "false".to_owned(),
        args: Vec::new(),
        extensions: vec!["rs".to_owned()],
        root_markers: Vec::new(),
        install_options: Vec::new(),
        diagnostics: DiagnosticMode::Push,
    };
    let mut settings = CodeDiagnosticsSettings::default();
    settings.custom_adapters.push(adapter.clone());
    let broker = Arc::new(Mutex::new(DiagnosticBroker::new(
        project.path(),
        vec![adapter],
        settings,
    )));
    let database = graph.dashboard_database_guard();
    let mut indexed_files = graph.get_all_file_paths().await.expect("indexed files");
    indexed_files.sort();
    assert_eq!(indexed_files, vec!["fixture.rs"]);
    let authority = DashboardDiagnosticsAuthorityV1::new(
        project.path().to_path_buf(),
        project.path().to_path_buf(),
        database,
        Arc::clone(&broker),
    );

    let error = authority
        .refresh_language("fake")
        .await
        .expect_err("an analyzer process failure must fail the refresh");
    assert!(matches!(error, DashboardDiagnosticsErrorV1::Runtime(_)));

    let progress = broker
        .lock()
        .await
        .snapshot()
        .backfill
        .get("fake")
        .cloned()
        .expect("backfill progress");
    assert_eq!(progress.queued_files, 1);
    assert_eq!(progress.opened_files, 0);
    assert_eq!(progress.last_completed_sweep, None);
}
