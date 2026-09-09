use tracedecay_code_extraction::BashExtractor;
use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_domain::*;

#[test]
fn test_bash_call_sites() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.sh").unwrap();
    let extractor = BashExtractor;
    let result = extractor.extract("sample.sh", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let call_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(!call_refs.is_empty(), "should have call refs");
    assert!(
        call_refs.iter().any(|r| r.reference_name == "echo"),
        "should find echo call"
    );
    assert!(
        call_refs.iter().any(|r| r.reference_name == "log"),
        "should find log call"
    );
    assert!(
        call_refs.iter().any(|r| r.reference_name == "curl"),
        "should find curl call"
    );
    assert!(
        call_refs
            .iter()
            .any(|r| r.reference_name == "validate_config"),
        "should find validate_config call"
    );
}

#[test]
fn test_bash_docstrings() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.sh").unwrap();
    let extractor = BashExtractor;
    let result = extractor.extract("sample.sh", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log")
        .expect("log function not found");
    assert!(
        log_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("Logs a message"),
        "docstring: {:?}",
        log_fn.docstring
    );

    let connect_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "connect")
        .expect("connect function not found");
    assert!(
        connect_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("Connects to the remote server"),
        "docstring: {:?}",
        connect_fn.docstring
    );

    let main_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "main")
        .expect("main function not found");
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
fn test_bash_contains_edges() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.sh").unwrap();
    let extractor = BashExtractor;
    let result = extractor.extract("sample.sh", &source);
    let contains: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains)
        .collect();
    assert!(
        contains.len() >= 8,
        "should have >= 8 Contains edges, got {}",
        contains.len()
    );
}
