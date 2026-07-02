//! Protocol responses for the degraded startup server.
//!
//! When `tracedecay serve` cannot resolve a project at startup it must not
//! exit — MCP hosts (Cursor especially) never retry a failed server spawn,
//! so a startup exit permanently breaks the scope. Instead, `crate::serve`
//! runs a degraded loop that completes the handshake and answers every tool
//! call with an actionable notice. This module owns that loop's protocol
//! responses; every payload shape and the method dispatch are shared with the
//! full server ([`super::server`]) so the two surfaces cannot drift.

use serde_json::json;

use super::server::{classify_mcp_method, initialize_result, resources_list_result, McpMethod};
use super::transport::{ErrorCode, JsonRpcRequest, JsonRpcResponse};

/// Whether a raw input line is a `tools/call` request. The degraded loop
/// retries project resolution on exactly these lines.
pub(crate) fn is_tools_call_line(line: &str) -> bool {
    serde_json::from_str::<JsonRpcRequest>(line)
        .is_ok_and(|request| classify_mcp_method(&request.method) == McpMethod::ToolsCall)
}

/// Builds the degraded response for one raw input line, or `None` when the
/// line is a notification that takes no response.
///
/// The dispatch mirrors [`super::server::McpServer::handle_request`] method
/// for method via the shared [`classify_mcp_method`]: the handshake and the
/// static payloads (`tools/list`, `resources/list`) are the real ones, while
/// anything that would need a resolved project answers with `notice`.
pub(crate) fn degraded_response_for_line(line: &str, notice: &str) -> Option<JsonRpcResponse> {
    let request: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(e) => {
            return Some(JsonRpcResponse::error(
                serde_json::Value::Null,
                ErrorCode::ParseError,
                format!("failed to parse JSON-RPC request: {e}"),
            ));
        }
    };
    let kind = classify_mcp_method(&request.method);
    let id = request.id?;
    let response = match kind {
        // The notice doubles as the advertised instructions so clients that
        // surface them show the remediation right from the handshake.
        McpMethod::Initialize => JsonRpcResponse::success(id, initialize_result(notice)),
        McpMethod::InitializedAck | McpMethod::HookEvent => return None,
        // The full tool catalog is advertised so the host caches the real
        // tool surface; each call then explains the degraded state.
        McpMethod::ToolsList => {
            JsonRpcResponse::success(id, json!({ "tools": super::tools::get_tool_definitions() }))
        }
        // Tool execution errors are reported inside the result (`isError`)
        // per the MCP spec, so the agent sees the remediation text.
        McpMethod::ToolsCall => JsonRpcResponse::success(
            id,
            json!({
                "content": [{ "type": "text", "text": notice }],
                "isError": true,
            }),
        ),
        McpMethod::ResourcesList => JsonRpcResponse::success(id, resources_list_result()),
        McpMethod::ResourcesRead => {
            JsonRpcResponse::error(id, ErrorCode::InternalError, notice.to_string())
        }
        McpMethod::TrivialAck => JsonRpcResponse::success(id, json!({})),
        McpMethod::Unknown => JsonRpcResponse::error(
            id,
            ErrorCode::MethodNotFound,
            format!("method not found: {}", request.method),
        ),
    };
    Some(response)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::Value;

    const NOTICE: &str = "degraded test notice";

    fn response(line: &str) -> Option<JsonRpcResponse> {
        degraded_response_for_line(line, NOTICE)
    }

    fn result(line: &str) -> Value {
        response(line).unwrap().result.unwrap()
    }

    #[test]
    fn initialize_uses_the_shared_handshake_payload_with_the_notice() {
        let result = result(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        assert_eq!(result, initialize_result(NOTICE));
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "tracedecay");
        assert_eq!(result["instructions"], NOTICE);
    }

    #[test]
    fn tools_call_reports_the_notice_as_an_in_result_error() {
        let result = result(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"tracedecay_context","arguments":{}}}"#,
        );
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["content"][0]["text"], NOTICE);
    }

    #[test]
    fn tools_list_and_resources_list_serve_the_real_catalogs() {
        let tools = result(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#);
        assert!(
            !tools["tools"].as_array().unwrap().is_empty(),
            "the degraded server must advertise the real tool catalog"
        );
        let resources = result(r#"{"jsonrpc":"2.0","id":4,"method":"resources/list"}"#);
        assert_eq!(resources, resources_list_result());
    }

    #[test]
    fn notifications_and_initialized_acks_take_no_response() {
        assert!(response(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
        assert!(response(r#"{"jsonrpc":"2.0","id":5,"method":"initialized"}"#).is_none());
        // A tools/call notification (no id) cannot be answered either.
        assert!(
            response(r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"x"}}"#).is_none()
        );
    }

    #[test]
    fn unknown_methods_and_parse_errors_mirror_the_full_server() {
        let unknown = response(r#"{"jsonrpc":"2.0","id":6,"method":"bogus/method"}"#).unwrap();
        assert_eq!(
            unknown.error.unwrap().code,
            ErrorCode::MethodNotFound.as_i32()
        );
        let parse = response("not json").unwrap();
        assert_eq!(parse.error.unwrap().code, ErrorCode::ParseError.as_i32());
    }

    #[test]
    fn tools_call_lines_are_recognized_for_resolution_retries() {
        assert!(is_tools_call_line(
            r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"x"}}"#
        ));
        assert!(!is_tools_call_line(
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/list"}"#
        ));
        assert!(!is_tools_call_line("not json"));
    }
}
