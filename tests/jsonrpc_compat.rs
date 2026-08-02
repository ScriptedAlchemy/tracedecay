use serde_json::json;
use tracedecay::mcp::transport::{
    ErrorCode as RootErrorCode, JsonRpcRequest as RootJsonRpcRequest,
    JsonRpcResponse as RootJsonRpcResponse,
};
use tracedecay_jsonrpc::{ErrorCode, JsonRpcRequest, JsonRpcResponse};

#[test]
fn root_transport_reexports_jsonrpc_serialization_contract() {
    let request: JsonRpcRequest = serde_json::from_value(json!({
        "jsonrpc": "2.0",
        "id": null,
        "method": "tools/list"
    }))
    .unwrap();
    assert_eq!(request.id, Some(serde_json::Value::Null));

    let response = JsonRpcResponse::error(
        json!(7),
        ErrorCode::MethodNotFound,
        "unknown method".to_string(),
    );
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({
            "jsonrpc": "2.0",
            "id": 7,
            "error": {"code": -32601, "message": "unknown method"}
        })
    );

    let _: RootJsonRpcRequest = request;
    let _: RootJsonRpcResponse = response;
    assert_eq!(RootErrorCode::MethodNotFound.as_i32(), -32601);
}
