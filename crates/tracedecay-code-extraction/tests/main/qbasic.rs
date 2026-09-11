use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::QBasicExtractor;
use tracedecay_domain::*;

fn extract_fixture() -> ExtractionResult {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.qb").unwrap();
    let extractor = QBasicExtractor;
    let result = extractor.extract("sample.qb", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    result
}

#[test]
fn test_qbasic_type_fields() {
    let result = extract_fixture();
    let fields: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field && n.qualified_name.contains("Endpoint"))
        .collect();
    assert!(
        fields.len() >= 3,
        "expected >= 3 fields in Endpoint (host, port, connected), got {}: {:?}",
        fields.len(),
        fields.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert!(
        fields.iter().any(|n| n.name == "host"),
        "host field not found"
    );
    assert!(
        fields.iter().any(|n| n.name == "port"),
        "port field not found"
    );
    assert!(
        fields.iter().any(|n| n.name == "connected"),
        "connected field not found"
    );
}

#[test]
fn test_qbasic_call_sites() {
    let result = extract_fixture();
    let calls: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(!calls.is_empty(), "expected call site refs");

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

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "LogMessage")
        .expect("LogMessage function not found");
    assert!(
        log_fn.docstring.is_some(),
        "LogMessage should have docstring"
    );
    assert!(
        log_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("Logs a message"),
        "docstring: {:?}",
        log_fn.docstring
    );

    let validate_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "ValidateConfig")
        .expect("ValidateConfig function not found");
    assert!(
        validate_fn.docstring.is_some(),
        "ValidateConfig should have docstring"
    );

    let connect_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "ConnectServer")
        .expect("ConnectServer function not found");
    assert!(
        connect_fn.docstring.is_some(),
        "ConnectServer should have docstring"
    );

    let disconnect_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "DisconnectServer")
        .expect("DisconnectServer function not found");
    assert!(
        disconnect_fn.docstring.is_some(),
        "DisconnectServer should have docstring"
    );

    let is_connected_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "IsConnected")
        .expect("IsConnected function not found");
    assert!(
        is_connected_fn.docstring.is_some(),
        "IsConnected should have docstring"
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
    let dim_fields: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field && !n.qualified_name.contains("Endpoint"))
        .collect();
    assert!(
        dim_fields.len() >= 3,
        "expected >= 3 DIM SHARED fields (conn, logLevel, logMsg), got {}: {:?}",
        dim_fields.len(),
        dim_fields.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert!(
        dim_fields.iter().any(|n| n.name == "conn"),
        "conn field not found"
    );
    assert!(
        dim_fields.iter().any(|n| n.name == "logLevel"),
        "logLevel field not found"
    );
    assert!(
        dim_fields.iter().any(|n| n.name == "logMsg"),
        "logMsg field not found"
    );
}
