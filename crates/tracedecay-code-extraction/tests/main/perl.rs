use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::PerlExtractor;
use tracedecay_domain::*;

#[test]
fn test_perl_call_sites() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.pl").unwrap();
    let extractor = PerlExtractor;
    let result = extractor.extract_artifact("sample.pl", &source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let call_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    // connect method calls main::log_message (qualified call)
    assert!(
        call_refs
            .iter()
            .any(|r| r.reference_name == "main::log_message"),
        "should find main::log_message call, got: {:?}",
        call_refs
            .iter()
            .map(|r| &r.reference_name)
            .collect::<Vec<_>>()
    );

    assert!(
        call_refs
            .iter()
            .any(|r| r.reference_name == "Connection->new"),
        "should find Connection->new call, got: {:?}",
        call_refs
            .iter()
            .map(|r| &r.reference_name)
            .collect::<Vec<_>>()
    );

    assert!(
        call_refs
            .iter()
            .any(|r| r.reference_name == "$conn->connect"),
        "should find $conn->connect call, got: {:?}",
        call_refs
            .iter()
            .map(|r| &r.reference_name)
            .collect::<Vec<_>>()
    );

    assert!(
        call_refs.iter().any(|r| r.reference_name == "croak"),
        "should find croak call, got: {:?}",
        call_refs
            .iter()
            .map(|r| &r.reference_name)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_perl_docstrings() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.pl").unwrap();
    let result = PerlExtractor.extract_artifact("sample.pl", &source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let docs: Vec<(&str, &str)> = result
        .nodes
        .iter()
        .filter_map(|n| Some((n.name.as_str(), n.docstring.as_deref()?)))
        .collect();
    assert_eq!(
        docs,
        [
            ("MAX_RETRIES", "Maximum number of retries."),
            ("DEFAULT_PORT", "Default port for connections."),
            ("log_message", "Logs a message with the given level."),
            ("new", "Creates a new Connection object."),
            ("connect", "Connects to the remote host."),
            ("disconnect", "Disconnects from the remote host."),
            ("is_connected", "Checks if the connection is active."),
            ("new", "Creates a new Pool."),
            ("acquire", "Acquires a connection from the pool."),
        ]
    );
}

#[test]
fn test_perl_signatures() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.pl").unwrap();
    let extractor = PerlExtractor;
    let result = extractor.extract_artifact("sample.pl", &source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log_message")
        .expect("log_message function not found");
    let sig = log_fn.signature.as_ref().unwrap();
    assert!(
        sig.contains("sub") && sig.contains("log_message"),
        "log_message signature should contain sub and name, got: {}",
        sig
    );
}
