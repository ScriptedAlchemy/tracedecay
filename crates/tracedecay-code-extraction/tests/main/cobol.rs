use tracedecay_code_extraction::CobolExtractor;
use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_domain::*;

fn extract_fixture() -> ExtractionResult {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.cob").unwrap();
    let extractor = CobolExtractor;
    let result = extractor.extract_artifact("sample.cob", &source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    result
}

#[test]
fn test_cobol_data_items_as_fields_and_consts() {
    let result = extract_fixture();
    // Items with VALUE clause -> Const: WS-MAX-RETRIES, WS-DEFAULT-PORT, WS-CONNECTED, WS-RETRY-COUNT
    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert!(
        consts.iter().any(|n| n.name == "WS-MAX-RETRIES"),
        "WS-MAX-RETRIES const not found"
    );
    assert!(
        consts.iter().any(|n| n.name == "WS-DEFAULT-PORT"),
        "WS-DEFAULT-PORT const not found"
    );

    // Items without VALUE clause -> Field: WS-HOST, WS-PORT, WS-LOG-LEVEL, WS-LOG-MESSAGE
    let fields: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field)
        .collect();
    assert!(
        fields.iter().any(|n| n.name == "WS-HOST"),
        "WS-HOST field not found"
    );
    assert!(
        fields.iter().any(|n| n.name == "WS-PORT"),
        "WS-PORT field not found"
    );
    assert!(
        fields.iter().any(|n| n.name == "WS-LOG-LEVEL"),
        "WS-LOG-LEVEL field not found"
    );
    assert!(
        fields.iter().any(|n| n.name == "WS-LOG-MESSAGE"),
        "WS-LOG-MESSAGE field not found"
    );
}

#[test]
fn test_cobol_perform_calls() {
    let result = extract_fixture();
    let calls: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(
        calls.iter().any(|r| r.reference_name == "VALIDATE-CONFIG"),
        "expected call to VALIDATE-CONFIG, got: {:?}",
        calls.iter().map(|r| &r.reference_name).collect::<Vec<_>>()
    );
    assert!(
        calls.iter().any(|r| r.reference_name == "CONNECT-SERVER"),
        "expected call to CONNECT-SERVER"
    );
    assert!(
        calls
            .iter()
            .any(|r| r.reference_name == "DISCONNECT-SERVER"),
        "expected call to DISCONNECT-SERVER"
    );
    assert!(
        calls.iter().any(|r| r.reference_name == "LOG-MESSAGE"),
        "expected call to LOG-MESSAGE"
    );
}

#[test]
fn test_cobol_docstrings() {
    let result = extract_fixture();
    let docs: Vec<(&str, &str)> = result
        .nodes
        .iter()
        .filter_map(|n| Some((n.name.as_str(), n.docstring.as_deref()?)))
        .collect();
    assert_eq!(
        docs,
        [
            ("WS-MAX-RETRIES", "Maximum number of retries."),
            ("WS-DEFAULT-PORT", "Default port number."),
            ("WS-HOST", "Connection host name."),
            ("WS-PORT", "Connection port."),
            ("WS-CONNECTED", "Connection status flag."),
            ("WS-LOG-LEVEL", "Log level."),
            ("WS-LOG-MESSAGE", "Log message text."),
            ("WS-RETRY-COUNT", "Retry counter."),
            ("VALIDATE-CONFIG", "Validates the configuration."),
            ("LOG-MESSAGE", "Logs a message with timestamp."),
            ("CONNECT-SERVER", "Connects to the remote server."),
            ("DISCONNECT-SERVER", "Disconnects from the server."),
        ]
    );
}

#[test]
fn test_cobol_qualified_names() {
    let result = extract_fixture();
    let validate = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "VALIDATE-CONFIG")
        .unwrap();
    assert!(
        validate.qualified_name.contains("NETWORKING"),
        "VALIDATE-CONFIG qualified name should contain 'NETWORKING', got: {}",
        validate.qualified_name
    );
}
