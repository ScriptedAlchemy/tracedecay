//! JSON-RPC 2.0 protocol types and the line-oriented transport contract.
//!
//! This crate owns no I/O, admission, or daemon authority — concrete
//! transports live with their runtime.

#![forbid(unsafe_code)]

use serde::{Deserialize, Deserializer, Serialize};

const JSON_RPC_VERSION: &str = "2.0";

fn deserialize_request_id<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<serde_json::Value>, D::Error>
where
    D: Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// Protocol version; must be `"2.0"`.
    pub jsonrpc: String,
    /// Request identifier. May be a number, string, or null.
    /// Absent for notifications.
    #[serde(
        default,
        deserialize_with = "deserialize_request_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    /// Protocol version; always `"2.0"`.
    pub jsonrpc: String,
    pub id: serde_json::Value,
    /// Present on success; absent on error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Present on failure; absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl JsonRpcResponse {
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: serde_json::Value, code: ErrorCode, message: String) -> Self {
        Self::error_with_data(id, code, message, None)
    }

    pub fn error_with_data(
        id: serde_json::Value,
        code: ErrorCode,
        message: String,
        data: Option<serde_json::Value>,
    ) -> Self {
        Self {
            jsonrpc: JSON_RPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: code.as_i32(),
                message,
                data,
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    RequestCancelled,
    InternalError,
}

impl ErrorCode {
    pub fn as_i32(self) -> i32 {
        match self {
            Self::ParseError => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::RequestCancelled => -32800,
            Self::InternalError => -32603,
        }
    }
}

// ---------------------------------------------------------------------------
// Transport abstraction (zero-cost via monomorphization)
// ---------------------------------------------------------------------------

/// Implementations are monomorphized at each call site — no dyn dispatch.
pub trait McpTransport {
    /// Implementations MUST be cancellation-safe: every server read loop races
    /// this future against shutdown, cancellation, and handler completion in a
    /// `tokio::select!`, so a dropped read must not lose bytes it already
    /// consumed. Buffered implementations satisfy this by keeping the
    /// partial-frame accumulator in the transport (see
    /// `tracedecay_framing::BoundedLineReader`) rather than in the future.
    /// Returns `None` on EOF.
    fn read_line(
        &mut self,
    ) -> impl std::future::Future<Output = std::io::Result<Option<String>>> + Send;

    /// Write a complete line, including the trailing newline.
    fn write_line(
        &mut self,
        line: &str,
    ) -> impl std::future::Future<Output = std::io::Result<()>> + Send;

    fn flush(&mut self) -> impl std::future::Future<Output = std::io::Result<()>> + Send;

    /// Wait until a peer fully closes the connection. A read-side EOF is not
    /// sufficient: one-shot clients legitimately half-close after writing
    /// their request and still wait for the response. Callers may start this
    /// wait before EOF while setup work is pending. Transports without a
    /// native full-close signal leave this future pending.
    fn peer_fully_closed_after_eof(
        &self,
    ) -> impl std::future::Future<Output = ()> + Send + 'static {
        std::future::pending()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use serde_json::json;

    use super::*;

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
    fn test_parse_notification_without_id() {
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
    fn test_serialize_success_response() {
        let response =
            JsonRpcResponse::success(serde_json::Value::Number(1.into()), json!({"tools": []}));

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"tools\":[]"));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn test_serialize_error_response() {
        let response = JsonRpcResponse::error(
            serde_json::Value::Number(1.into()),
            ErrorCode::MethodNotFound,
            "Method not found".to_string(),
        );

        let json = serde_json::to_string(&response).unwrap();
        assert!(json.contains("-32601"));
        assert!(json.contains("Method not found"));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn test_error_codes() {
        assert_eq!(ErrorCode::ParseError.as_i32(), -32700);
        assert_eq!(ErrorCode::InvalidRequest.as_i32(), -32600);
        assert_eq!(ErrorCode::MethodNotFound.as_i32(), -32601);
        assert_eq!(ErrorCode::InvalidParams.as_i32(), -32602);
        assert_eq!(ErrorCode::RequestCancelled.as_i32(), -32800);
        assert_eq!(ErrorCode::InternalError.as_i32(), -32603);
    }

    #[test]
    fn test_request_with_string_id() {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": "abc-123",
            "method": "ping"
        });

        let request: JsonRpcRequest = serde_json::from_value(msg).unwrap();
        assert_eq!(
            request.id,
            Some(serde_json::Value::String("abc-123".to_string()))
        );
    }
}
