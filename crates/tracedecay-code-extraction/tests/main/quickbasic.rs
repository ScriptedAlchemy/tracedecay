#[cfg(feature = "lang-qbasic")]
mod quickbasic_tests {

    use tracedecay_code_extraction::LanguageExtractor;
    use tracedecay_code_extraction::QuickBasicExtractor;
    use tracedecay_domain::*;

    include!("support/edges.rs");

    fn extract_fixture() -> ExtractionResult {
        let source = std::fs::read_to_string("../../tests/fixtures/sample.bi").unwrap();
        let extractor = QuickBasicExtractor;
        let result = extractor.extract_artifact("sample.bi", &source).result;
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        result
    }

    #[test]
    fn test_quickbasic_file_node() {
        let result = extract_fixture();
        let files: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::File)
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "sample.bi");
    }

    #[test]
    fn test_quickbasic_sub_functions() {
        let result = extract_fixture();
        let fns: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(fns, ["InitSystem", "Shutdown", "GetStatus", "LogInit"]);
    }

    #[test]
    fn test_quickbasic_type_as_struct() {
        let result = extract_fixture();
        let structs: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Struct)
            .collect();
        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name, "Config");
    }

    #[test]
    fn test_quickbasic_type_fields() {
        let result = extract_fixture();
        let fields: Vec<&str> = edge_pairs(&result, EdgeKind::Contains)
            .into_iter()
            .filter(|(parent, _)| *parent == "Config")
            .map(|(_, child)| child)
            .collect();
        assert_eq!(fields, ["name", "value", "active"]);
    }

    #[test]
    fn test_quickbasic_const_nodes() {
        let result = extract_fixture();
        let consts: Vec<&str> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Const)
            .map(|n| n.name.as_str())
            .collect();
        assert_eq!(consts, ["VERSION", "MAX_ITEMS"]);
    }

    #[test]
    fn test_quickbasic_call_sites() {
        let result = extract_fixture();
        let calls: Vec<_> = result
            .unresolved_refs
            .iter()
            .filter(|r| r.reference_kind == EdgeKind::Calls)
            .collect();
        assert!(
            calls.iter().any(|r| r.reference_name == "LogInit"),
            "expected CALL LogInit from InitSystem"
        );
    }

    #[test]
    fn test_quickbasic_complexity() {
        let result = extract_fixture();
        let get_status = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Function && n.name == "GetStatus")
            .expect("GetStatus function not found");
        assert_eq!(get_status.branches, 1);
    }

    #[test]
    fn test_quickbasic_docstrings() {
        let result = extract_fixture();
        let docs: Vec<(&str, &str)> = result
            .nodes
            .iter()
            .filter_map(|n| Some((n.name.as_str(), n.docstring.as_deref()?)))
            .collect();
        assert_eq!(
            docs,
            [
                ("InitSystem", "Initializes the system."),
                ("Shutdown", "Shuts down the system."),
                ("GetStatus", "Returns the current status."),
                ("LogInit", "Logs initialization.")
            ]
        );
    }

    #[test]
    fn test_quickbasic_contains_edges() {
        let result = extract_fixture();
        assert_eq!(
            edge_pairs(&result, EdgeKind::Contains),
            [
                ("sample.bi", "VERSION"),
                ("sample.bi", "MAX_ITEMS"),
                ("sample.bi", "Config"),
                ("Config", "name"),
                ("Config", "value"),
                ("Config", "active"),
                ("sample.bi", "appConfig"),
                ("sample.bi", "InitSystem"),
                ("sample.bi", "Shutdown"),
                ("sample.bi", "GetStatus"),
                ("sample.bi", "LogInit")
            ]
        );
    }

    #[test]
    fn test_quickbasic_parses_redim_and_sleep() {
        let source = r#"
SUB Test
    REDIM arr(1 TO 10) AS INTEGER
    SLEEP 1
    ERASE arr
END SUB
"#;
        let extractor = QuickBasicExtractor;
        let result = extractor.extract_artifact("test.bi", source).result;
        assert!(
            result.errors.is_empty(),
            "QB4.5 statements should parse without errors: {:?}",
            result.errors
        );
        let fns: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Function)
            .collect();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "Test");
    }
} // mod quickbasic_tests
