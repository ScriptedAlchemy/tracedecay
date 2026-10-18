use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::LuaExtractor;
use tracedecay_domain::*;

#[test]
fn test_lua_call_sites() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.lua").unwrap();
    let extractor = LuaExtractor;
    let result = extractor.extract_artifact("sample.lua", &source).result;
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let call_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls)
        .collect();
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
    let result = LuaExtractor.extract_artifact("sample.lua", &source).result;
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
            (
                "log",
                "Logs a message with the given level.\n\
                 @param level string The log level\n\
                 @param message string The message to log"
            ),
            (
                "new",
                "Creates a new Connection.\n\
                 @param host string The host to connect to\n\
                 @param port number The port number\n\
                 @return Connection"
            ),
            ("connect", "Connects to the remote host."),
            ("disconnect", "Disconnects from the remote host."),
            ("isConnected", "Checks if the connection is active."),
        ]
    );
}

#[test]
fn test_lua_local_function_is_private() {
    let source = std::fs::read_to_string("../../tests/fixtures/sample.lua").unwrap();
    let extractor = LuaExtractor;
    let result = extractor.extract_artifact("sample.lua", &source).result;
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
    let result = extractor.extract_artifact("sample.lua", &source).result;
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
    let result = extractor.extract_artifact("sample.lua", &source).result;
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
