use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::PowerShellExtractor;
use tracedecay_domain::*;

#[test]
fn test_powershell_call_sites() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.ps1").unwrap();
    let extractor = PowerShellExtractor;
    let result = extractor.extract("sample.ps1", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let call_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(!call_refs.is_empty(), "should have call refs");
    assert!(
        call_refs.iter().any(|r| r.reference_name == "Write-Host"),
        "should find Write-Host call"
    );
    assert!(
        call_refs.iter().any(|r| r.reference_name == "Write-Log"),
        "should find Write-Log call"
    );
    assert!(
        call_refs
            .iter()
            .any(|r| r.reference_name == "Test-Connection"),
        "should find Test-Connection call"
    );
    assert!(
        call_refs.iter().any(|r| r.reference_name == "Test-Config"),
        "should find Test-Config call"
    );
}

#[test]
fn test_powershell_docstrings() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.ps1").unwrap();
    let extractor = PowerShellExtractor;
    let result = extractor.extract("sample.ps1", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    // Write-Log should have a block comment docstring.
    let write_log = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "Write-Log")
        .expect("Write-Log function not found");
    assert!(
        write_log
            .docstring
            .as_ref()
            .unwrap()
            .contains("Logs a message"),
        "docstring: {:?}",
        write_log.docstring
    );

    // Test-Config should have a line comment docstring.
    let test_config = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "Test-Config")
        .expect("Test-Config function not found");
    assert!(
        test_config
            .docstring
            .as_ref()
            .unwrap()
            .contains("Validates the configuration"),
        "docstring: {:?}",
        test_config.docstring
    );

    let main_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "Main")
        .expect("Main function not found");
    assert!(
        main_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("Main entry point"),
        "docstring: {:?}",
        main_fn.docstring
    );
}

#[test]
fn test_powershell_contains_edges() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.ps1").unwrap();
    let extractor = PowerShellExtractor;
    let result = extractor.extract("sample.ps1", &source);
    let contains: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains)
        .collect();
    assert!(
        contains.len() >= 9,
        "should have >= 9 Contains edges, got {}",
        contains.len()
    );
}
