//! Typed status resource rendering.

use serde_json::{Value, json};
use tracedecay_mcp::handlers::info::graph_statistics_value;

use super::{ErrorCode, JsonRpcResponse, McpServer};

impl McpServer {
    /// Returns project identity and typed graph-statistics availability.
    #[hotpath::skip]
    pub(crate) async fn read_resource_status(&self, id: Value) -> JsonRpcResponse {
        let cg = self.reopen_if_branch_drifted().await;
        let census = match self.generation_census_reader() {
            Some(reader) => Some(reader().await),
            None => None,
        };
        let graph_statistics = match graph_statistics_value(census.as_ref()) {
            Ok(value) => value,
            Err(error) => {
                return JsonRpcResponse::error(
                    id,
                    ErrorCode::InternalError,
                    format!("failed to serialize graph statistics: {error}"),
                );
            }
        };
        let output = json!({
            "project_root": cg.project_root(),
            "branch_diagnostics": cg.branch_diagnostics(),
            "graph_statistics": graph_statistics,
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
