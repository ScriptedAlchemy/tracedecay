use tracedecay_code_extraction::FortranExtractor;
use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_domain::*;

fn extract_fixture() -> ExtractionResult {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.f90").unwrap();
    let extractor = FortranExtractor;
    let result = extractor.extract_artifact("sample.f90", &source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    result
}

#[test]
fn test_fortran_module() {
    let result = extract_fixture();
    let modules: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .collect();
    assert_eq!(
        modules.len(),
        1,
        "expected 1 module, got {}: {:?}",
        modules.len(),
        modules.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert_eq!(modules[0].name, "networking");
}

#[test]
fn test_fortran_interface() {
    let result = extract_fixture();
    let interfaces: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Interface)
        .collect();
    assert_eq!(
        interfaces.len(),
        1,
        "expected 1 interface, got {}: {:?}",
        interfaces.len(),
        interfaces.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert_eq!(interfaces[0].name, "Connectable");
}

#[test]
fn test_fortran_fields() {
    let result = extract_fixture();
    let fields: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field)
        .collect();
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
    assert!(
        fields.iter().any(|n| n.name == "pool_size"),
        "pool_size field not found"
    );
}

#[test]
fn test_fortran_call_sites() {
    let result = extract_fixture();
    let calls: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(
        calls.iter().any(|r| r.reference_name == "log_message"),
        "expected call to log_message, got: {:?}",
        calls.iter().map(|r| &r.reference_name).collect::<Vec<_>>()
    );
    assert!(
        calls.iter().any(|r| r.reference_name == "create_endpoint"),
        "expected call to create_endpoint"
    );
    assert!(
        calls.iter().any(|r| r.reference_name == "connect_endpoint"),
        "expected call to connect_endpoint"
    );
    assert!(
        calls
            .iter()
            .any(|r| r.reference_name == "disconnect_endpoint"),
        "expected call to disconnect_endpoint"
    );
}

#[test]
fn test_fortran_docstrings() {
    let result = extract_fixture();
    let docs: Vec<(&str, &str)> = result
        .nodes
        .iter()
        .filter_map(|n| Some((n.name.as_str(), n.docstring.as_deref()?)))
        .collect();
    assert_eq!(
        docs,
        [
            (
                "networking",
                "Sample Fortran file exercising extractor features."
            ),
            ("MAX_RETRIES", "Maximum number of retries."),
            ("DEFAULT_PORT", "Default port for connections."),
            ("Endpoint", "Represents a network endpoint."),
            (
                "PooledEndpoint",
                "Extends Endpoint with pool functionality."
            ),
            ("Connectable", "Interface for connectable types."),
            ("log_message", "Logs a message with the given level."),
            ("create_endpoint", "Creates a new endpoint."),
            ("connect_endpoint", "Connects an endpoint."),
            ("disconnect_endpoint", "Disconnects an endpoint."),
            ("is_connected", "Checks if endpoint is connected."),
        ]
    );
}

#[test]
fn test_fortran_qualified_names() {
    let result = extract_fixture();
    let log_msg = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log_message")
        .unwrap();
    assert!(
        log_msg.qualified_name.contains("networking"),
        "log_message qualified name should contain 'networking', got: {}",
        log_msg.qualified_name
    );
}
