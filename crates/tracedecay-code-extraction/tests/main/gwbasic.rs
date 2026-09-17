use tracedecay_code_extraction::GwBasicExtractor;
use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_domain::*;

fn extract_fixture() -> ExtractionResult {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.gw").unwrap();
    let extractor = GwBasicExtractor;
    let result = extractor.extract("sample.gw", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    result
}

#[test]
fn test_gwbasic_gosub_calls() {
    let result = extract_fixture();
    let calls: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(!calls.is_empty(), "expected call site refs");

    assert!(
        calls.iter().any(|r| r.reference_name == "1000"),
        "expected GOSUB 1000 call, got: {:?}",
        calls.iter().map(|r| &r.reference_name).collect::<Vec<_>>()
    );
    assert!(
        calls.iter().any(|r| r.reference_name == "2000"),
        "expected GOSUB 2000 call"
    );
    assert!(
        calls.iter().any(|r| r.reference_name == "3000"),
        "expected GOSUB 3000 call"
    );
}

#[test]
fn test_gwbasic_docstrings() {
    let result = extract_fixture();

    let validate_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "VALIDATE_CONFIGURATION")
        .expect("VALIDATE_CONFIGURATION function not found");
    assert!(
        validate_fn.docstring.is_some(),
        "VALIDATE_CONFIGURATION should have docstring"
    );
    assert!(
        validate_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("VALIDATE CONFIGURATION"),
        "docstring: {:?}",
        validate_fn.docstring
    );

    let connect_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "CONNECT_TO_SERVER")
        .expect("CONNECT_TO_SERVER function not found");
    assert!(
        connect_fn.docstring.is_some(),
        "CONNECT_TO_SERVER should have docstring"
    );

    let disconnect_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "DISCONNECT")
        .expect("DISCONNECT function not found");
    assert!(
        disconnect_fn.docstring.is_some(),
        "DISCONNECT should have docstring"
    );
}

#[test]
fn test_gwbasic_subroutine_complexity() {
    let result = extract_fixture();

    let connect_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "CONNECT_TO_SERVER")
        .expect("CONNECT_TO_SERVER function not found");
    assert!(
        connect_fn.loops >= 1,
        "CONNECT_TO_SERVER should have >= 1 loop, got {}",
        connect_fn.loops
    );

    let validate_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "VALIDATE_CONFIGURATION")
        .expect("VALIDATE_CONFIGURATION function not found");
    assert!(
        validate_fn.branches >= 1,
        "VALIDATE_CONFIGURATION should have >= 1 branch, got {}",
        validate_fn.branches
    );
}

#[test]
fn test_gwbasic_subroutine_signatures() {
    let result = extract_fixture();

    // Subroutine signatures should contain the GOSUB target line number.
    let validate_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "VALIDATE_CONFIGURATION")
        .expect("VALIDATE_CONFIGURATION function not found");
    assert!(
        validate_fn.signature.as_ref().unwrap().contains("GOSUB"),
        "signature should contain GOSUB: {:?}",
        validate_fn.signature
    );
}
