use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::MsBasic2Extractor;
use tracedecay_domain::*;

fn extract_fixture() -> ExtractionResult {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.bas").unwrap();
    let extractor = MsBasic2Extractor;
    let result = extractor.extract_artifact("sample.bas", &source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    result
}

#[test]
fn test_msbasic2_gosub_calls() {
    let result = extract_fixture();
    let calls: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(
        calls.iter().any(|r| r.reference_name == "200"),
        "expected GOSUB 200 call, got: {:?}",
        calls.iter().map(|r| &r.reference_name).collect::<Vec<_>>()
    );
    assert!(
        calls.iter().any(|r| r.reference_name == "300"),
        "expected GOSUB 300 call"
    );
    assert!(
        calls.iter().any(|r| r.reference_name == "400"),
        "expected GOSUB 400 call"
    );
}

#[test]
fn test_msbasic2_docstrings() {
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
                "LOG_A_MESSAGE",
                "LOG A MESSAGE\nPARAMS: L$=LEVEL, M$=MESSAGE"
            ),
            ("CONNECT_TO_SERVER", "CONNECT TO SERVER"),
            ("DISCONNECT", "DISCONNECT"),
        ]
    );
}

#[test]
fn test_msbasic2_subroutine_signatures() {
    let result = extract_fixture();

    // Subroutine signatures should contain the GOSUB target line number.
    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "LOG_A_MESSAGE")
        .expect("LOG_A_MESSAGE function not found");
    assert!(
        log_fn.signature.as_ref().unwrap().contains("GOSUB"),
        "signature should contain GOSUB: {:?}",
        log_fn.signature
    );
}

#[test]
fn test_msbasic2_subroutine_internal_calls() {
    let result = extract_fixture();
    let calls: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();

    let gosub_200_count = calls.iter().filter(|r| r.reference_name == "200").count();
    assert!(
        gosub_200_count >= 3,
        "expected >= 3 GOSUB 200 calls (top-level + connect + disconnect), got {}",
        gosub_200_count
    );
}

#[test]
fn test_msbasic2_let_name_keeps_underscores() {
    let result = MsBasic2Extractor
        .extract_artifact("names.bas", "10 LET MAX_RETRIES = 3\n20 LET MR = 1\n")
        .result;
    let consts: Vec<&str> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(consts, ["MAX_RETRIES", "MR"]);
}
