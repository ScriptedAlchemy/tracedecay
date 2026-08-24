use std::sync::Arc;

use serde_json::json;

use crate::branch::BranchAddOutcome;
use crate::errors::TraceDecayError;
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse};
use crate::tracedecay::TraceDecay;

use super::{DaemonHandshake, StoreAdministration, open_project_for_handshake};

const BRANCH_ADD_TOOL_NAME: &str = "tracedecay_admin_branch_add";

pub(super) fn coordinated_hook_branch_writer(
    administration: StoreAdministration,
) -> crate::mcp::server::HookBranchWriter {
    Arc::new(move |request| {
        let administration = administration.clone();
        Box::pin(async move {
            administration
                .with_writer(|| async move {
                    crate::mcp::server::execute_hook_branch_write_direct(request).await
                })
                .await
        })
    })
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
    administration: &StoreAdministration,
    handshake: &DaemonHandshake,
    request: &BranchAddRequest,
) -> JsonRpcResponse {
    let branch = match request.branch.as_deref() {
        Ok(branch) => branch,
        Err(message) => {
            return JsonRpcResponse::error(
                request.id.clone(),
                ErrorCode::InvalidParams,
                message.clone(),
            );
        }
    };

    let result = administration
        .with_writer(|| async {
            let project_root =
                handshake
                    .project_path
                    .as_deref()
                    .ok_or_else(|| TraceDecayError::Config {
                        message: "branch add requires a project path".to_string(),
                    })?;
            let cg = open_project_for_handshake(project_root, handshake).await?;
            let outcome = TraceDecay::add_branch_tracking_with_options(
                cg.project_root(),
                branch,
                cg.open_options(),
            )
            .await;
            drop(cg);
            outcome
        })
        .await;

    match result {
        Ok(outcome) => {
            JsonRpcResponse::success(request.id.clone(), branch_add_tool_result(&outcome))
        }
        Err(error) => JsonRpcResponse::error(
            request.id.clone(),
            ErrorCode::InternalError,
            error.to_string(),
        ),
    }
}

fn branch_add_tool_result(outcome: &BranchAddOutcome) -> serde_json::Value {
    let output = json!({ "outcome": branch_add_outcome_name(outcome) });
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string(&output).unwrap_or_default(),
        }]
    })
}

fn branch_add_outcome_name(outcome: &BranchAddOutcome) -> &'static str {
    match outcome {
        BranchAddOutcome::NotIndexed => "not_indexed",
        BranchAddOutcome::AlreadyTracked => "already_tracked",
        BranchAddOutcome::Added => "added",
        BranchAddOutcome::Deferred => "deferred",
    }
}
