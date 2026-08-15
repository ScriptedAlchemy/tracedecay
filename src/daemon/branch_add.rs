use crate::branch::BranchAddOutcome;
use crate::errors::TraceDecayError;
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse};

use super::code_index_scheduler::CodeIndexSchedulerRegistryV1;
use super::{DaemonHandshake, StoreAdministration};

const BRANCH_ADD_TOOL_NAME: &str = "tracedecay_admin_branch_add";
const CODE_INDEX_SCHEDULER_UNAVAILABLE: &str = "code_index_scheduler_unavailable";
const PROJECT_PATH_UNAVAILABLE: &str = "project_path_unavailable";

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
    administration: &StoreAdministration,
    schedulers: Option<&CodeIndexSchedulerRegistryV1>,
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

    if schedulers.is_none() {
        return typed_project_route_error(
            request.id.clone(),
            CODE_INDEX_SCHEDULER_UNAVAILABLE,
            true,
            "code-index scheduler authority is unavailable for branch activation",
        );
    }

    let Some(project_root) = handshake.project_path.as_deref() else {
        return typed_project_route_error(
            request.id.clone(),
            PROJECT_PATH_UNAVAILABLE,
            false,
            "branch add requires a project path",
        );
    };
    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mounted = administration.mounted_project_graphs().await;
    let Some(graph) = mounted
        .iter()
        .find(|graph| graph_matches_project(graph, &canonical_root))
        .cloned()
    else {
        return typed_project_route_error(
            request.id.clone(),
            CODE_INDEX_SCHEDULER_UNAVAILABLE,
            true,
            "retained branch-add graph is unavailable",
        );
    };

    #[cfg(unix)]
    {
        match super::pr_autotrack::activate_manual_branch_head(
            &canonical_root,
            &graph,
            schedulers,
            branch,
        )
        .await
        {
            Ok(activation) => JsonRpcResponse::success(
                request.id.clone(),
                branch_add_tool_result(&activation.outcome),
            ),
            Err(error) => typed_project_route_error(
                request.id.clone(),
                error.reason_code(),
                error.retryable(),
                error.detail(),
            ),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = (administration, graph, branch);
        typed_project_route_error(
            request.id.clone(),
            CODE_INDEX_SCHEDULER_UNAVAILABLE,
            true,
            "code-index scheduler authority is unavailable for branch activation",
        )
    }
}

fn graph_matches_project(
    graph: &crate::tracedecay::TraceDecay,
    canonical_root: &std::path::Path,
) -> bool {
    graph.project_root() == canonical_root
        || graph
            .project_root()
            .canonicalize()
            .ok()
            .is_some_and(|root| root == canonical_root)
}

fn typed_project_route_error(
    id: serde_json::Value,
    reason_code: &str,
    retryable: bool,
    detail: &str,
) -> JsonRpcResponse {
    let error = if reason_code == CODE_INDEX_SCHEDULER_UNAVAILABLE {
        scheduler_unavailable(detail)
    } else {
        TraceDecayError::project_route(reason_code, retryable, detail)
    };
    JsonRpcResponse::error_with_data(
        id,
        ErrorCode::InternalError,
        error.to_string(),
        Some(serde_json::json!({
            "reason_code": reason_code,
            "retryable": retryable,
            "detail": detail,
        })),
    )
}

fn branch_add_tool_result(outcome: &BranchAddOutcome) -> serde_json::Value {
    let name = branch_add_outcome_name(outcome);
    serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!(r#"{{"outcome":"{name}"}}"#),
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
