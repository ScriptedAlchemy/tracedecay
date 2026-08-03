use tracedecay_lsp::{adapters, broker, client, settings};

#[test]
fn lsp_public_modules_are_available() {
    assert!(!adapters::builtin_adapters().is_empty());
    let _ = broker::DiagnosticBroker::new_for_test(".", Vec::new());
    let _ = client::LspRefreshTimeouts::from_diagnostics_quiet_window(
        std::time::Duration::from_secs(1),
    );
    assert_eq!(
        settings::CodeDiagnosticsSettings::default().languages.len(),
        0
    );
}
