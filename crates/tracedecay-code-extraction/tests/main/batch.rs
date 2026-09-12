use tracedecay_code_extraction::BatchExtractor;
use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_domain::*;

#[test]
fn test_batch_call_sites() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.bat").unwrap();
    let extractor = BatchExtractor;
    let result = extractor.extract("sample.bat", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let call_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(!call_refs.is_empty(), "should have call refs");
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
    let result = extractor.extract("sample.bat", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "Log")
        .expect("Log function not found");
    assert!(log_fn.docstring.is_some(), "Log should have docstring");
    assert!(
        log_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("Logs a message"),
        "docstring: {:?}",
        log_fn.docstring
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

#[test]
fn test_batch_contains_edges() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.bat").unwrap();
    let extractor = BatchExtractor;
    let result = extractor.extract("sample.bat", &source);
    let contains: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains)
        .collect();
    assert!(
        contains.len() >= 7,
        "should have >= 7 Contains edges, got {}",
        contains.len()
    );
}
