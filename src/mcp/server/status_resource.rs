//! Typed status resource rendering.

use serde_json::{Value, json};

use super::{ErrorCode, JsonRpcResponse, McpServer};

impl McpServer {
    /// Returns project identity and typed graph-statistics availability.
    pub(crate) async fn read_resource_status(&self, id: Value) -> JsonRpcResponse {
        let cg = self.reopen_if_branch_drifted().await;
        let output = json!({
            "project_root": cg.project_root(),
            "branch_diagnostics": cg.branch_diagnostics(),
            "graph_statistics": {
                "status": "unavailable",
                "reason": "sealed_generation_statistics_not_published",
            },
        });
        match serde_json::to_string_pretty(&output) {
            Ok(text) => {
                Self::resource_contents(id, "tracedecay://status", "application/json", &text)
            }
            Err(error) => JsonRpcResponse::error(
                id,
                ErrorCode::InternalError,
                format!("failed to serialize project status: {error}"),
            ),
        }
    }
}
