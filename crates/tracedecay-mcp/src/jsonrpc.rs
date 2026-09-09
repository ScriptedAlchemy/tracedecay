//! JSON-RPC 2.0 protocol types and the line-oriented transport contract.
//!
//! This crate owns no I/O, admission, or daemon authority — concrete
//! transports live with their runtime.

#![forbid(unsafe_code)]

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use serde_json::error::Category;

const JSON_RPC_VERSION: &str = "2.0";

fn deserialize_request_id<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<serde_json::Value>, D::Error>
where
    D: Deserializer<'de>,
{
    serde_json::Value::deserialize(deserializer).map(Some)
}

/// Why a wire line is not a JSON-RPC 2.0 message.
///
/// JSON-RPC 2.0 separates text that is not JSON (`ParseError`, `id: null`)
/// from JSON that is not a valid message object (`InvalidRequest`, correlated
/// to the object's `id` member when it has one). Every transport rejects
/// through this one type, so the native and rmcp connections agree on which
/// side of that line a frame falls and on the id the rejection carries.
#[derive(Debug)]
pub enum JsonRpcDecodeError {
    /// The line is not JSON.
    Parse(serde_json::Error),
    /// The line is JSON but not a JSON-RPC 2.0 request or notification.
    InvalidRequest { id: Value, reason: String },
}

impl JsonRpcDecodeError {
    fn invalid_request(id: Value, reason: impl std::fmt::Display) -> Self {
        Self::InvalidRequest {
            id,
            reason: reason.to_string(),
        }
    }

    /// The rejection frame a transport writes back for the undecodable line.
    pub fn into_response(self) -> JsonRpcResponse {
        match self {
            Self::Parse(error) => JsonRpcResponse::error(
                Value::Null,
                ErrorCode::ParseError,
                format!("failed to parse JSON-RPC request: {error}"),
            ),
            Self::InvalidRequest { id, reason } => JsonRpcResponse::error(
                id,
                ErrorCode::InvalidRequest,
                format!("invalid JSON-RPC request: {reason}"),
            ),
        }
    }
}

impl std::fmt::Display for JsonRpcDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "failed to parse JSON-RPC request: {error}"),
            Self::InvalidRequest { reason, .. } => write!(f, "invalid JSON-RPC request: {reason}"),
        }
    }
}

impl std::error::Error for JsonRpcDecodeError {}

/// The `id` a rejection of this parsed message correlates to: the object's
/// `id` member when present, otherwise `null` as JSON-RPC 2.0 requires when
/// the id cannot be determined.
fn detected_request_id(value: &Value) -> Value {
    value.get("id").cloned().unwrap_or(Value::Null)
}

/// Rejects every parsed JSON value that is not a JSON-RPC 2.0 message
/// envelope: an object whose `jsonrpc` member is exactly the string `"2.0"`.
///
/// This is the whole envelope rule. It runs before any method
/// classification, cancellation matching, or dispatch, so a frame with a
/// different protocol version is never interpreted as work.
pub fn validate_envelope(value: &Value) -> std::result::Result<(), JsonRpcDecodeError> {
    let Some(object) = value.as_object() else {
        return Err(JsonRpcDecodeError::invalid_request(
            Value::Null,
            "message must be a JSON object",
        ));
    };
    match object.get("jsonrpc") {
        Some(Value::String(version)) if version == JSON_RPC_VERSION => Ok(()),
        Some(version) => Err(JsonRpcDecodeError::invalid_request(
            detected_request_id(value),
            format!("jsonrpc must be \"{JSON_RPC_VERSION}\", got {version}"),
        )),
        None => Err(JsonRpcDecodeError::invalid_request(
            detected_request_id(value),
            "missing jsonrpc member",
        )),
    }
}

/// Decodes an already-parsed JSON value into a typed JSON-RPC 2.0 message
/// after [`validate_envelope`]. Transports that parse to [`Value`] first (the
/// rmcp receive loop) use this so their shape rejections carry the same id
/// correlation as [`JsonRpcRequest::decode`].
pub fn decode_envelope<T: DeserializeOwned>(
    value: Value,
) -> std::result::Result<T, JsonRpcDecodeError> {
    validate_envelope(&value)?;
    let id = detected_request_id(&value);
    serde_json::from_value(value).map_err(|error| JsonRpcDecodeError::invalid_request(id, error))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    /// Protocol version; must be `"2.0"`. Serde accepts any string here so
    /// that [`JsonRpcRequest::decode`] can answer a wrong version with
    /// `InvalidRequest` carrying the request's id; production transports
    /// decode only through that constructor.
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

impl JsonRpcRequest {
    /// Decodes one wire line as a JSON-RPC 2.0 request or notification.
    ///
    /// Single-pass on the accepted path: the line is deserialized straight
    /// into the request and only its `jsonrpc` member is checked afterwards.
    /// A line that is valid JSON but not a request object is re-read as a
    /// [`Value`] solely to recover the `id` the rejection must carry.
    pub fn decode(line: &str) -> std::result::Result<Self, JsonRpcDecodeError> {
        let request = match serde_json::from_str::<Self>(line) {
            Ok(request) => request,
            Err(error) if error.classify() == Category::Data => {
                let id = serde_json::from_str::<Value>(line)
                    .map_or(Value::Null, |value| detected_request_id(&value));
                return Err(JsonRpcDecodeError::invalid_request(id, error));
            }
            Err(error) => return Err(JsonRpcDecodeError::Parse(error)),
        };
        if request.jsonrpc != JSON_RPC_VERSION {
            return Err(JsonRpcDecodeError::invalid_request(
                request.id.unwrap_or(Value::Null),
                format!(
                    "jsonrpc must be \"{JSON_RPC_VERSION}\", got \"{}\"",
                    request.jsonrpc
                ),
            ));
        }
        Ok(request)
    }
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

    fn invalid_request_id(error: JsonRpcDecodeError) -> Value {
        match error {
            JsonRpcDecodeError::InvalidRequest { id, .. } => id,
            JsonRpcDecodeError::Parse(error) => {
                panic!("expected InvalidRequest, got Parse({error})")
            }
        }
    }

    #[test]
    fn decode_accepts_exact_version_for_requests_and_notifications() {
        let request =
            JsonRpcRequest::decode(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).unwrap();
        assert_eq!(request.method, "tools/list");
        assert_eq!(request.id, Some(json!(1)));

        let notification =
            JsonRpcRequest::decode(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
                .unwrap();
        assert!(notification.id.is_none());
    }

    #[test]
    fn decode_rejects_every_non_exact_version_with_the_request_id() {
        for version in [
            json!("1.0"),
            json!("2.1"),
            json!(""),
            json!("garbage"),
            json!(2.0),
        ] {
            let line = json!({
                "jsonrpc": version,
                "id": 7,
                "method": "tools/call",
                "params": {"name": "tracedecay_status", "arguments": {}}
            })
            .to_string();
            let error = JsonRpcRequest::decode(&line).unwrap_err();
            assert_eq!(invalid_request_id(error), json!(7), "version {version}");
        }
    }

    #[test]
    fn decode_rejects_missing_version_as_invalid_request() {
        let error = JsonRpcRequest::decode(r#"{"id":"abc","method":"ping"}"#).unwrap_err();
        assert_eq!(invalid_request_id(error), json!("abc"));
    }

    #[test]
    fn decode_rejects_wrong_version_notification_with_null_id() {
        let error =
            JsonRpcRequest::decode(r#"{"jsonrpc":"1.0","method":"notifications/initialized"}"#)
                .unwrap_err();
        assert_eq!(invalid_request_id(error), Value::Null);
    }

    #[test]
    fn decode_keeps_malformed_json_as_parse_error() {
        for line in ["this is not json {{{", "", "{\"jsonrpc\":\"2.0\",\"id\":1,"] {
            assert!(
                matches!(
                    JsonRpcRequest::decode(line),
                    Err(JsonRpcDecodeError::Parse(_))
                ),
                "line {line:?}"
            );
        }
        // Valid JSON that is not an object is invalid, not unparseable.
        assert_eq!(
            invalid_request_id(JsonRpcRequest::decode("[]").unwrap_err()),
            Value::Null
        );
    }

    #[test]
    fn decode_error_responses_carry_protocol_codes_and_ids() {
        let parse = JsonRpcDecodeError::Parse(serde_json::from_str::<Value>("{").unwrap_err())
            .into_response();
        assert_eq!(parse.id, Value::Null);
        assert_eq!(parse.error.unwrap().code, ErrorCode::ParseError.as_i32());

        let invalid = JsonRpcRequest::decode(r#"{"jsonrpc":"1.0","id":9,"method":"ping"}"#)
            .unwrap_err()
            .into_response();
        assert_eq!(invalid.id, json!(9));
        let error = invalid.error.unwrap();
        assert_eq!(error.code, ErrorCode::InvalidRequest.as_i32());
        assert!(
            error.message.contains("jsonrpc must be \"2.0\""),
            "{}",
            error.message
        );
    }

    #[test]
    fn decode_envelope_applies_the_same_rule_to_parsed_values() {
        let value = json!({"jsonrpc": "2.0", "id": 3, "method": "ping"});
        assert!(validate_envelope(&value).is_ok());
        let request: JsonRpcRequest = decode_envelope(value).unwrap();
        assert_eq!(request.method, "ping");

        let foreign = json!({"jsonrpc": "1.0", "id": 3, "method": "ping"});
        assert_eq!(
            invalid_request_id(validate_envelope(&foreign).unwrap_err()),
            json!(3)
        );
        assert_eq!(
            invalid_request_id(decode_envelope::<JsonRpcRequest>(foreign).unwrap_err()),
            json!(3)
        );
        assert_eq!(
            invalid_request_id(validate_envelope(&json!({"id": 4, "method": "ping"})).unwrap_err()),
            json!(4)
        );
        assert_eq!(
            invalid_request_id(validate_envelope(&json!("2.0")).unwrap_err()),
            Value::Null
        );
        // A valid envelope whose body is not a request still correlates by id.
        assert_eq!(
            invalid_request_id(
                decode_envelope::<JsonRpcRequest>(json!({"jsonrpc": "2.0", "id": 5})).unwrap_err()
            ),
            json!(5)
        );
    }
}
