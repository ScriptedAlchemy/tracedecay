use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::PerlExtractor;
use tracedecay_domain::*;

#[test]
fn test_perl_call_sites() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.pl").unwrap();
    let extractor = PerlExtractor;
    let result = extractor.extract("sample.pl", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let call_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(!call_refs.is_empty(), "should have call refs");

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
    let extractor = PerlExtractor;
    let result = extractor.extract("sample.pl", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log_message")
        .expect("log_message function not found");
    assert!(
        log_fn.docstring.is_some(),
        "log_message should have docstring"
    );
    let doc = log_fn.docstring.as_ref().unwrap();
    assert!(
        doc.contains("Logs a message"),
        "docstring should contain 'Logs a message', got: {}",
        doc
    );

    let max_retries = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Const && n.name == "MAX_RETRIES")
        .expect("MAX_RETRIES not found");
    assert!(
        max_retries
            .docstring
            .as_ref()
            .unwrap()
            .contains("Maximum number of retries"),
        "docstring: {:?}",
        max_retries.docstring
    );

    let connect = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Method && n.name == "connect")
        .expect("connect method not found");
    assert!(
        connect
            .docstring
            .as_ref()
            .unwrap()
            .contains("Connects to the remote host"),
        "docstring: {:?}",
        connect.docstring
    );
}

#[test]
fn test_perl_signatures() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.pl").unwrap();
    let extractor = PerlExtractor;
    let result = extractor.extract("sample.pl", &source);
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
