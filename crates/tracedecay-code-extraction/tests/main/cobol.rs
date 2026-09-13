use tracedecay_code_extraction::CobolExtractor;
use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_domain::*;

fn extract_fixture() -> ExtractionResult {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.cob").unwrap();
    let extractor = CobolExtractor;
    let result = extractor.extract("sample.cob", &source);
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
    assert!(!calls.is_empty(), "expected call site refs");
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
    let validate = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "VALIDATE-CONFIG");
    assert!(validate.is_some(), "VALIDATE-CONFIG not found");
    assert!(
        validate.unwrap().docstring.is_some(),
        "VALIDATE-CONFIG should have docstring"
    );

    let log_msg = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "LOG-MESSAGE");
    assert!(log_msg.is_some(), "LOG-MESSAGE not found");
    assert!(
        log_msg.unwrap().docstring.is_some(),
        "LOG-MESSAGE should have docstring"
    );

    let connect = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "CONNECT-SERVER");
    assert!(connect.is_some(), "CONNECT-SERVER not found");
    assert!(
        connect.unwrap().docstring.is_some(),
        "CONNECT-SERVER should have docstring"
    );

    let disconnect = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "DISCONNECT-SERVER");
    assert!(disconnect.is_some(), "DISCONNECT-SERVER not found");
    assert!(
        disconnect.unwrap().docstring.is_some(),
        "DISCONNECT-SERVER should have docstring"
    );

    let max_retries = result.nodes.iter().find(|n| n.name == "WS-MAX-RETRIES");
    assert!(max_retries.is_some(), "WS-MAX-RETRIES not found");
    assert!(
        max_retries.unwrap().docstring.is_some(),
        "WS-MAX-RETRIES should have docstring"
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
