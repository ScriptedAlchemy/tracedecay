use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::QBasicExtractor;
use tracedecay_domain::*;

fn extract_fixture() -> ExtractionResult {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.qb").unwrap();
    let extractor = QBasicExtractor;
    let result = extractor.extract_artifact("sample.qb", &source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    result
}

#[test]
fn test_qbasic_type_fields() {
    let result = extract_fixture();
    let fields: Vec<&str> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field && n.qualified_name.contains("Endpoint"))
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(fields, ["host", "port", "connected"]);
}

#[test]
fn test_qbasic_call_sites() {
    let result = extract_fixture();
    let calls: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(
        calls.iter().any(|r| r.reference_name == "ValidateConfig"),
        "expected CALL ValidateConfig, got: {:?}",
        calls.iter().map(|r| &r.reference_name).collect::<Vec<_>>()
    );
    assert!(
        calls.iter().any(|r| r.reference_name == "ConnectServer"),
        "expected CALL ConnectServer"
    );
    assert!(
        calls.iter().any(|r| r.reference_name == "DisconnectServer"),
        "expected CALL DisconnectServer"
    );

    assert!(
        calls.iter().any(|r| r.reference_name == "LogMessage"),
        "expected CALL LogMessage from within SUBs"
    );
}

#[test]
fn test_qbasic_docstrings() {
    let result = extract_fixture();
    let docs: Vec<(&str, &str)> = result
        .nodes
        .iter()
        .filter_map(|n| Some((n.name.as_str(), n.docstring.as_deref()?)))
        .collect();
    assert_eq!(
        docs,
        [
            ("LogMessage", "Logs a message with the given level."),
            ("ValidateConfig", "Validates the configuration."),
            ("ConnectServer", "Connects to the remote server."),
            ("DisconnectServer", "Disconnects from the server."),
            ("IsConnected", "Checks if connected."),
        ]
    );
}

#[test]
fn test_qbasic_signatures() {
    let result = extract_fixture();

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "LogMessage")
        .expect("LogMessage function not found");
    assert!(
        log_fn.signature.as_ref().unwrap().contains("SUB"),
        "LogMessage signature should contain SUB: {:?}",
        log_fn.signature
    );

    let is_connected_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "IsConnected")
        .expect("IsConnected function not found");
    assert!(
        is_connected_fn
            .signature
            .as_ref()
            .unwrap()
            .contains("FUNCTION"),
        "IsConnected signature should contain FUNCTION: {:?}",
        is_connected_fn.signature
    );
}

#[test]
fn test_qbasic_dim_shared_fields() {
    let result = extract_fixture();
    let dim_fields: Vec<&str> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field && !n.qualified_name.contains("Endpoint"))
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(dim_fields, ["conn", "logLevel", "logMsg"]);
}

#[test]
fn test_qbasic_names_keep_underscores() {
    let source = "CONST MAX_RETRIES = 3\nCONST DEFAULT_PORT = 8080\n";
    let result = QBasicExtractor.extract_artifact("names.qb", source).result;
    let named: Vec<(&NodeKind, &str)> = result
        .nodes
        .iter()
        .filter(|n| n.kind != NodeKind::File)
        .map(|n| (&n.kind, n.name.as_str()))
        .collect();
    assert_eq!(
        named,
        [
            (&NodeKind::Const, "MAX_RETRIES"),
            (&NodeKind::Const, "DEFAULT_PORT")
        ]
    );
}
