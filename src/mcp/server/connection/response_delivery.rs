use serde_json::Value;

use crate::errors::Result;
use crate::mcp::server::McpServer;
use crate::mcp::server::routing::SelectedProjectResponseLease;
use crate::mcp::server::tool_errors::serialize_response_line;
use crate::mcp::transport::{ErrorCode, JsonRpcRequest, JsonRpcResponse, McpTransport};

impl McpServer {
    /// Process a single raw JSON-RPC line and write the response.
    /// Used to replay a peeked `initialize` message that was consumed before
    /// the server's main loop started.
    pub async fn handle_and_write(
        &self,
        line: &str,
        transport: &mut impl McpTransport,
    ) -> Result<()> {
        let parsed: std::result::Result<JsonRpcRequest, _> = serde_json::from_str(line);
        let project_tool_call = parsed
            .as_ref()
            .is_ok_and(|request| request.method == "tools/call")
            && self.project_server_live.is_some();
        let project_request_admitted = !project_tool_call
            || !self
                .project_server_lifecycle
                .response_revoked()
                .is_cancelled();
        let mut connection = self.new_connection_route_state()?;
        let response = if !project_request_admitted {
            parsed.as_ref().ok().and_then(|request| {
                request.id.clone().map(|id| {
                    JsonRpcResponse::error_with_data(
                        id,
                        ErrorCode::InternalError,
                        "tool project route failed: project server was retired".to_owned(),
                        Some(serde_json::json!({
                            "reason_code": "project_server_retired",
                            "retryable": true,
                            "detail": "the retained project server was replaced or revoked; retry against the current owner",
                        })),
                    )
                })
            })
        } else {
            match parsed {
                Ok(request) => {
                    Box::pin(self.handle_request_for_connection(
                        &request,
                        self.timings_enabled(),
                        &mut connection,
                        false,
                    ))
                    .await
                }
                Err(error) => Some(JsonRpcResponse::error(
                    Value::Null,
                    ErrorCode::ParseError,
                    format!("failed to parse JSON-RPC request: {error}"),
                )),
            }
        };
        let selected_response_lease = connection.take_selected_response_lease();
        let response_revoked = selected_response_lease
            .as_ref()
            .map(SelectedProjectResponseLease::revoked);
        if let Some(response) = response {
            let mut json_line = serialize_response_line(&response);
            json_line.push('\n');
            let _ = self
                .write_response_line_or_revoke(transport, &json_line, response_revoked)
                .await?;
        }
        Ok(())
    }
}
