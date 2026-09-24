use tracedecay_code_extraction::BatchExtractor;
use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_domain::*;

#[test]
fn test_batch_call_sites() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.bat").unwrap();
    let extractor = BatchExtractor;
    let result = extractor.extract_artifact("sample.bat", &source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let call_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(
        call_refs.iter().any(|r| r.reference_name == "Log"),
        "should find Log call"
    );
    assert!(
        call_refs
            .iter()
            .any(|r| r.reference_name == "ValidateConfig"),
        "should find ValidateConfig call"
    );
    assert!(
        call_refs.iter().any(|r| r.reference_name == "Connect"),
        "should find Connect call"
    );
    assert!(
        call_refs.iter().any(|r| r.reference_name == "Disconnect"),
        "should find Disconnect call"
    );
}

#[test]
fn test_batch_docstrings() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.bat").unwrap();
    let extractor = BatchExtractor;
    let result = extractor.extract_artifact("sample.bat", &source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "Log")
        .expect("Log function not found");
    assert_eq!(
        log_fn.docstring.as_deref(),
        Some("Logs a message with timestamp.")
    );

    let vc_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "ValidateConfig")
        .expect("ValidateConfig function not found");
    assert!(
        vc_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("Validates the configuration"),
        "docstring: {:?}",
        vc_fn.docstring
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
