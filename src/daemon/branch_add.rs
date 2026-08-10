use crate::errors::TraceDecayError;
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse};

use super::{DaemonHandshake, StoreAdministration};

const BRANCH_ADD_TOOL_NAME: &str = "tracedecay_admin_branch_add";
const CODE_INDEX_SCHEDULER_UNAVAILABLE: &str = "code_index_scheduler_unavailable";

fn scheduler_unavailable(detail: impl Into<String>) -> TraceDecayError {
    TraceDecayError::project_route(CODE_INDEX_SCHEDULER_UNAVAILABLE, true, detail)
}

pub(super) struct BranchAddRequest {
    pub(super) id: serde_json::Value,
    branch: std::result::Result<String, String>,
}

pub(super) fn parse_branch_add_request(line: &str) -> Option<BranchAddRequest> {
    let request = serde_json::from_str::<JsonRpcRequest>(line.trim()).ok()?;
    if request.method != "tools/call" {
        return None;
    }
    let params = request.params.as_ref()?;
    if params.get("name").and_then(serde_json::Value::as_str) != Some(BRANCH_ADD_TOOL_NAME) {
        return None;
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let branch = arguments
        .get("branch")
        .and_then(serde_json::Value::as_str)
        .filter(|branch| !branch.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "missing required parameter: branch".to_string());
    Some(BranchAddRequest {
        id: request.id.unwrap_or(serde_json::Value::Null),
        branch,
    })
}

pub(super) async fn branch_add_response(
    _administration: &StoreAdministration,
    _handshake: &DaemonHandshake,
    request: &BranchAddRequest,
) -> JsonRpcResponse {
    let _branch = match request.branch.as_deref() {
        Ok(branch) => branch,
        Err(message) => {
            return JsonRpcResponse::error(
                request.id.clone(),
                ErrorCode::InvalidParams,
                message.clone(),
            );
        }
    };

    let detail = "code-index scheduler authority is unavailable for branch activation";
    let error = scheduler_unavailable(detail);
    JsonRpcResponse::error_with_data(
        request.id.clone(),
        ErrorCode::InternalError,
        error.to_string(),
        Some(serde_json::json!({
            "reason_code": CODE_INDEX_SCHEDULER_UNAVAILABLE,
            "retryable": true,
            "detail": detail,
        })),
    )
}
