use tracedecay_code_extraction::GwBasicExtractor;
use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_domain::*;

fn extract_fixture() -> ExtractionResult {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.gw").unwrap();
    let extractor = GwBasicExtractor;
    let result = extractor.extract_artifact("sample.gw", &source).result;
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
    let docs: Vec<(&str, &str)> = result
        .nodes
        .iter()
        .filter_map(|n| Some((n.name.as_str(), n.docstring.as_deref()?)))
        .collect();
    assert_eq!(
        docs,
        [
            ("VALIDATE_CONFIGURATION", "VALIDATE CONFIGURATION"),
            ("CONNECT_TO_SERVER", "CONNECT TO SERVER"),
            ("DISCONNECT", "DISCONNECT"),
        ]
    );
}

#[test]
fn test_gwbasic_subroutine_complexity() {
    let result = extract_fixture();

    // VALIDATE_CONFIGURATION has two `IF`s, CONNECT_TO_SERVER one `WHILE`.
    let complexity: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .map(|n| (n.name.as_str(), n.branches, n.loops))
        .collect();
    assert_eq!(
        complexity,
        [
            ("FNLOG", 0, 0),
            ("VALIDATE_CONFIGURATION", 2, 0),
            ("CONNECT_TO_SERVER", 0, 1),
            ("DISCONNECT", 0, 0),
        ]
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

#[test]
fn test_gwbasic_let_name_keeps_underscores() {
    let result = GwBasicExtractor
        .extract_artifact("names.gw", "10 LET MAX_RETRIES = 3\n20 LET MR = 1\n")
        .result;
    let consts: Vec<&str> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(consts, ["MAX_RETRIES", "MR"]);
}
