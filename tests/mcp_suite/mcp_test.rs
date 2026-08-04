use serde_json::json;
use tracedecay::mcp::tools::*;
use tracedecay::mcp::transport::*;

#[test]
fn test_parse_jsonrpc_request() {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/list",
        "params": {}
    });

    let request: JsonRpcRequest = serde_json::from_value(msg).unwrap();
    assert_eq!(request.method, "tools/list");
    assert_eq!(request.id, Some(serde_json::Value::Number(1.into())));
}

#[test]
fn test_tool_definitions() {
    let tools = get_tool_definitions();
    assert!(!tools.is_empty());

    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(tool_names.contains(&"tracedecay_search"));
    assert!(tool_names.contains(&"tracedecay_context"));
    assert!(tool_names.contains(&"tracedecay_callers"));
    assert!(tool_names.contains(&"tracedecay_callees"));
    assert!(tool_names.contains(&"tracedecay_impact"));
    assert!(tool_names.contains(&"tracedecay_node"));
    assert!(tool_names.contains(&"tracedecay_status"));
    assert!(tool_names.contains(&"tracedecay_project_list"));
    assert!(tool_names.contains(&"tracedecay_project_search"));
    assert!(tool_names.contains(&"tracedecay_project_context"));
}

#[test]
fn test_serialize_jsonrpc_response() {
    let response = JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id: serde_json::Value::Number(1.into()),
        result: Some(json!({"tools": []})),
        error: None,
    };

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"jsonrpc\":\"2.0\""));
}

#[test]
fn test_error_response() {
    let response = JsonRpcResponse::error(
        serde_json::Value::Number(1.into()),
        ErrorCode::MethodNotFound,
        "Method not found".to_string(),
    );

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("-32601"));
}

#[test]
fn test_success_response_omits_error() {
    let response = JsonRpcResponse::success(
        serde_json::Value::Number(42.into()),
        json!({"result": "ok"}),
    );

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("\"result\""));
    assert!(!json.contains("\"error\""));
}

#[test]
fn test_error_response_omits_result() {
    let response = JsonRpcResponse::error(
        serde_json::Value::Number(1.into()),
        ErrorCode::InternalError,
        "something went wrong".to_string(),
    );

    let json = serde_json::to_string(&response).unwrap();
    assert!(json.contains("-32603"));
    assert!(!json.contains("\"result\""));
}

#[test]
fn test_all_error_codes() {
    assert_eq!(ErrorCode::ParseError.as_i32(), -32700);
    assert_eq!(ErrorCode::InvalidRequest.as_i32(), -32600);
    assert_eq!(ErrorCode::MethodNotFound.as_i32(), -32601);
    assert_eq!(ErrorCode::InvalidParams.as_i32(), -32602);
    assert_eq!(ErrorCode::InternalError.as_i32(), -32603);
}

#[test]
fn test_tool_definition_scope_properties_match_handlers() {
    let tools = get_tool_definitions();
    for tool in &tools {
        assert!(
            tool.input_schema["properties"].get("hermes_home").is_none(),
            "{} must not expose Hermes host-home routing",
            tool.name
        );
        let storage_scope = tool.input_schema["properties"].get("storage_scope");
        if tool.name.starts_with("tracedecay_lcm_") || tool.name == "tracedecay_message_search" {
            assert_eq!(storage_scope.unwrap()["enum"], json!(["project", "user"]));
        } else {
            assert!(
                storage_scope.is_none(),
                "{} exposes LCM storage scope",
                tool.name
            );
        }
    }
}

#[test]
fn test_ast_grep_tools_follow_capability_gates() {
    let tools = get_tool_definitions();
    let tool_names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();

    assert_eq!(
        tool_names.contains(&"tracedecay_ast_grep_rewrite"),
        tracedecay::mcp::tools::ast_grep_available(),
        "rewrite should be gated on the external ast-grep CLI"
    );
    // Structural search runs in-process (bundled grammars), so it is advertised
    // unconditionally — no host ast-grep CLI required.
    assert!(tool_names.contains(&"tracedecay_ast_grep_search"));
    assert!(tool_names.contains(&"tracedecay_outline"));
}

#[test]
fn test_write_and_exec_tools_are_not_read_only() {
    // Tools that mutate files or run subprocesses must advertise
    // `readOnlyHint: false`, otherwise harnesses that auto-approve read-only
    // tools will edit files / run `cargo test` without prompting. See #94.
    let write_or_exec = [
        "tracedecay_str_replace",
        "tracedecay_multi_str_replace",
        "tracedecay_insert_at",
        "tracedecay_replace_symbol",
        "tracedecay_insert_at_symbol",
        "tracedecay_move_symbol",
        "tracedecay_run_affected_tests",
        "tracedecay_ast_grep_rewrite",
        "tracedecay_lcm_doctor",
    ];
    // The only entry allowed to be missing: it is registered just when
    // ast-grep is on PATH. Skipping any other name would let a renamed or
    // dropped mutating tool pass this gate without ever being checked.
    const PATH_CONDITIONAL: &str = "tracedecay_ast_grep_rewrite";
    let tools = get_tool_definitions();
    for name in write_or_exec {
        let Some(tool) = tools.iter().find(|t| t.name == name) else {
            assert_eq!(
                name, PATH_CONDITIONAL,
                "write/exec tool '{name}' is no longer in the catalog; the \
                 readOnlyHint gate would silently skip it. Re-point this list \
                 at the tool's current name instead of dropping the coverage."
            );
            continue;
        };
        let annotations = tool
            .annotations
            .as_ref()
            .unwrap_or_else(|| panic!("tool '{name}' has no annotations"));
        assert_eq!(
            annotations["readOnlyHint"],
            serde_json::Value::Bool(false),
            "write/exec tool '{name}' must have readOnlyHint: false"
        );
    }
}

#[test]
fn test_tool_definitions_have_input_schemas() {
    let tools = get_tool_definitions();
    for tool in &tools {
        assert!(
            tool.input_schema.is_object(),
            "tool '{}' has no input schema",
            tool.name
        );
        assert_eq!(
            tool.input_schema["type"], "object",
            "tool '{}' schema type is not object",
            tool.name
        );
    }
}

#[test]
fn test_tool_definitions_serialization_roundtrip() {
    let tools = get_tool_definitions();
    let json = serde_json::to_string(&tools).unwrap();
    let deserialized: Vec<ToolDefinition> = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.len(), tools.len());
    for (orig, deser) in tools.iter().zip(deserialized.iter()) {
        assert_eq!(orig.name, deser.name);
        assert_eq!(orig.description, deser.description);
    }
}

#[test]
fn test_notification_without_id() {
    let msg = json!({
        "jsonrpc": "2.0",
        "method": "initialized"
    });

    let request: JsonRpcRequest = serde_json::from_value(msg).unwrap();
    assert_eq!(request.method, "initialized");
    assert!(request.id.is_none());
    assert!(request.params.is_none());
}

#[test]
fn test_serialize_request_omits_absent_id_but_preserves_null_id() {
    let notification = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: None,
        method: "initialized".to_string(),
        params: None,
    };
    let serialized = serde_json::to_value(&notification).unwrap();
    assert!(serialized.get("id").is_none());

    let request = JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id: Some(serde_json::Value::Null),
        method: "ping".to_string(),
        params: None,
    };
    let serialized = serde_json::to_value(&request).unwrap();
    assert!(serialized["id"].is_null());
}

#[test]
fn test_request_with_string_id() {
    let msg = json!({
        "jsonrpc": "2.0",
        "id": "req-42",
        "method": "ping"
    });

    let request: JsonRpcRequest = serde_json::from_value(msg).unwrap();
    assert_eq!(
        request.id,
        Some(serde_json::Value::String("req-42".to_string()))
    );
    assert_eq!(request.method, "ping");
}
