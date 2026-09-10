use tracedecay_code_extraction::BashExtractor;
use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::incremental::{
    ParseDocumentIdentity, ParseLimits, ParseReuse, RetainedParseDocument,
};
use tracedecay_code_extraction::parsed_extraction::{
    ParsedExtractionDisposition, ParsedExtractionResetReason,
};
use tracedecay_domain::*;

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Display,
{
    T::try_from(value.to_owned()).unwrap_or_else(|error| panic!("{value}: {error}"))
}

fn bash_overlay(version: i64, content: &str) -> ParseDocumentIdentity {
    ParseDocumentIdentity::SessionOverlay {
        scope_identity: id::<ManifestDigest>(&format!("sha256:{:064x}", 1)),
        document_identity: id::<ManifestDigest>(&format!("sha256:{:064x}", 2)),
        version,
        content_digest: id::<ContentDigest>(&format!("sha256:{content:0>64}")),
        logical_path: "usage.sh".to_owned(),
    }
}

#[test]
fn test_bash_extract_functions() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.sh").unwrap();
    let extractor = BashExtractor;
    let result = extractor.extract("sample.sh", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function)
        .collect();
    assert_eq!(
        fns.len(),
        5,
        "expected 5 functions, got {}: {:?}",
        fns.len(),
        fns.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert!(fns.iter().any(|n| n.name == "log"));
    assert!(fns.iter().any(|n| n.name == "validate_config"));
    assert!(fns.iter().any(|n| n.name == "connect"));
    assert!(fns.iter().any(|n| n.name == "disconnect"));
    assert!(fns.iter().any(|n| n.name == "main"));
}

#[test]
fn test_bash_extract_readonly_consts() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.sh").unwrap();
    let extractor = BashExtractor;
    let result = extractor.extract("sample.sh", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert_eq!(
        consts.len(),
        2,
        "expected 2 consts, got {}: {:?}",
        consts.len(),
        consts.iter().map(|n| &n.name).collect::<Vec<_>>()
    );
    assert!(consts.iter().any(|n| n.name == "MAX_RETRIES"));
    assert!(consts.iter().any(|n| n.name == "DEFAULT_PORT"));
}

#[test]
fn test_bash_extract_source_import() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.sh").unwrap();
    let extractor = BashExtractor;
    let result = extractor.extract("sample.sh", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let uses: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Use)
        .collect();
    assert_eq!(uses.len(), 1, "expected 1 Use node, got {}", uses.len());
    assert_eq!(uses[0].name, "./utils.sh");
}

#[test]
fn test_bash_call_sites() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.sh").unwrap();
    let extractor = BashExtractor;
    let result = extractor.extract("sample.sh", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let call_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    let script = result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Module && node.name == "sample")
        .expect("script execution scope");
    assert!(!call_refs.is_empty(), "should have call refs");
    assert!(
        call_refs.iter().any(|r| r.reference_name == "echo"),
        "should find echo call"
    );
    assert!(
        call_refs.iter().any(|r| r.reference_name == "log"),
        "should find log call"
    );
    assert!(
        call_refs.iter().any(|r| r.reference_name == "curl"),
        "should find curl call"
    );
    assert!(
        call_refs
            .iter()
            .any(|r| r.reference_name == "validate_config"),
        "should find validate_config call"
    );
    assert_eq!(
        call_refs
            .iter()
            .filter(|r| r.reference_name == "validate_config")
            .count(),
        1,
        "script-level extraction must skip commands inside function definitions"
    );
    assert!(
        call_refs
            .iter()
            .any(|r| { r.reference_name == "main" && r.from_node_id == script.id })
    );
    assert_eq!(
        result
            .edges
            .iter()
            .filter(|edge| {
                edge.kind == EdgeKind::Contains
                    && edge.source == script.id
                    && result
                        .nodes
                        .iter()
                        .any(|node| node.kind == NodeKind::Function && node.id == edge.target)
            })
            .count(),
        5
    );
}

#[test]
fn test_bash_incremental_edit_rebuilds_script_scope() {
    let before =
        "kept() { echo kept; }\noldfn() { echo old; }\nnewfn() { echo new; }\nkept\noldfn\n";
    let after =
        "kept() { echo kept; }\noldfn() { echo old; }\nnewfn() { echo new; }\nkept\nnewfn\n";
    let (mut document, opened) =
        RetainedParseDocument::open(bash_overlay(1, "1"), "bash", before, ParseLimits::default())
            .expect("initial Bash parse");
    let initial = document
        .extract_canonical(&BashExtractor, &opened, None)
        .expect("initial Bash extraction");

    let report = document
        .reparse(bash_overlay(2, "2"), after)
        .expect("incremental Bash parse");
    assert_eq!(report.reuse, ParseReuse::Incremental);
    let updated = document
        .extract_canonical(&BashExtractor, &report, Some(&initial.result))
        .expect("updated Bash extraction");
    assert_eq!(
        updated.disposition,
        ParsedExtractionDisposition::Reset {
            reason: ParsedExtractionResetReason::ChangedRootIdentity
        }
    );

    let script = updated
        .result
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Module)
        .expect("script module");
    let mut functions = updated
        .result
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Function)
        .map(|node| node.name.as_str())
        .collect::<Vec<_>>();
    functions.sort_unstable();
    assert_eq!(functions, ["kept", "newfn", "oldfn"]);
    let top_level_calls = updated
        .result
        .unresolved_refs
        .iter()
        .filter(|reference| reference.from_node_id == script.id)
        .map(|reference| reference.reference_name.as_str())
        .collect::<Vec<_>>();
    assert!(top_level_calls.contains(&"kept"));
    assert!(top_level_calls.contains(&"newfn"));
    assert!(!top_level_calls.contains(&"oldfn"));
}

#[test]
fn test_bash_docstrings() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.sh").unwrap();
    let extractor = BashExtractor;
    let result = extractor.extract("sample.sh", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log")
        .expect("log function not found");
    assert!(
        log_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("Logs a message"),
        "docstring: {:?}",
        log_fn.docstring
    );

    let connect_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "connect")
        .expect("connect function not found");
    assert!(
        connect_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("Connects to the remote server"),
        "docstring: {:?}",
        connect_fn.docstring
    );

    let main_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "main")
        .expect("main function not found");
    assert!(
        main_fn
            .docstring
            .as_ref()
            .unwrap()
            .contains("Main entry point"),
        "docstring: {:?}",
        main_fn.docstring
    );
}

#[test]
fn test_bash_file_node() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.sh").unwrap();
    let extractor = BashExtractor;
    let result = extractor.extract("sample.sh", &source);
    let files: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "sample.sh");
}

#[test]
fn test_bash_contains_edges() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.sh").unwrap();
    let extractor = BashExtractor;
    let result = extractor.extract("sample.sh", &source);
    let contains: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains)
        .collect();
    assert!(
        contains.len() >= 8,
        "should have >= 8 Contains edges, got {}",
        contains.len()
    );
}
