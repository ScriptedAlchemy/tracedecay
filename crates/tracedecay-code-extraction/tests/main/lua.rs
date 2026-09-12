use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::LuaExtractor;
use tracedecay_domain::*;

#[test]
fn test_lua_call_sites() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.lua").unwrap();
    let extractor = LuaExtractor;
    let result = extractor.extract("sample.lua", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let call_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
    assert!(!call_refs.is_empty(), "should have call refs");

    assert!(
        call_refs.iter().any(|r| r.reference_name == "print"),
        "should find print call"
    );
    assert!(
        call_refs
            .iter()
            .any(|r| r.reference_name == "string.format"),
        "should find string.format call"
    );

    assert!(
        call_refs.iter().any(|r| r.reference_name == "setmetatable"),
        "should find setmetatable call"
    );

    assert!(
        call_refs.iter().any(|r| r.reference_name == "log"),
        "should find log call"
    );

    assert!(
        call_refs
            .iter()
            .any(|r| r.reference_name == "Connection.new"),
        "should find Connection.new call"
    );
    assert!(
        call_refs.iter().any(|r| r.reference_name == "conn:connect"),
        "should find conn:connect call"
    );

    assert!(
        call_refs.iter().any(|r| r.reference_name == "table.insert"),
        "should find table.insert call"
    );
}

#[test]
fn test_lua_docstrings() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.lua").unwrap();
    let extractor = LuaExtractor;
    let result = extractor.extract("sample.lua", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log")
        .expect("log function not found");
    assert!(log_fn.docstring.is_some(), "log should have docstring");
    let doc = log_fn.docstring.as_ref().unwrap();
    assert!(
        doc.contains("Logs a message"),
        "docstring should contain 'Logs a message', got: {}",
        doc
    );

    let connect_method = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Method && n.name == "connect")
        .expect("connect method not found");
    assert!(
        connect_method
            .docstring
            .as_ref()
            .unwrap()
            .contains("Connects to the remote host"),
        "docstring: {:?}",
        connect_method.docstring
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
}

#[test]
fn test_lua_contains_edges() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.lua").unwrap();
    let extractor = LuaExtractor;
    let result = extractor.extract("sample.lua", &source);
    let contains: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains)
        .collect();
    assert!(
        contains.len() >= 12,
        "should have >= 12 Contains edges, got {}",
        contains.len()
    );
}

#[test]
fn test_lua_local_function_is_private() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.lua").unwrap();
    let extractor = LuaExtractor;
    let result = extractor.extract("sample.lua", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log")
        .expect("log function not found");
    assert_eq!(
        log_fn.visibility,
        Visibility::Private,
        "local function should be private"
    );
}

#[test]
fn test_lua_dot_function_qualified_name() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.lua").unwrap();
    let extractor = LuaExtractor;
    let result = extractor.extract("sample.lua", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let conn_new_fns: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name == "new")
        .collect();
    assert!(
        conn_new_fns
            .iter()
            .any(|n| n.qualified_name.contains("Connection")),
        "Connection.new should have Connection in qualified name, got: {:?}",
        conn_new_fns
            .iter()
            .map(|n| &n.qualified_name)
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_lua_signatures() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.lua").unwrap();
    let extractor = LuaExtractor;
    let result = extractor.extract("sample.lua", &source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let log_fn = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Function && n.name == "log")
        .expect("log function not found");
    let sig = log_fn.signature.as_ref().unwrap();
    assert!(
        sig.contains("function")
            && sig.contains("log")
            && sig.contains("level")
            && sig.contains("message"),
        "log signature should contain function name and params, got: {}",
        sig
    );

    let connect = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Method && n.name == "connect")
        .expect("connect not found");
    let sig = connect.signature.as_ref().unwrap();
    assert!(
        sig.contains("Connection:connect"),
        "connect signature should contain Connection:connect, got: {}",
        sig
    );
}
