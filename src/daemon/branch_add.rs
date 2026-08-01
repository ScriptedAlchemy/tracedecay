use std::sync::Arc;

use serde_json::json;

use crate::branch::BranchAddOutcome;
use crate::errors::TraceDecayError;
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse};

use super::{DaemonHandshake, StoreAdministration};

const BRANCH_ADD_TOOL_NAME: &str = "tracedecay_admin_branch_add";

pub(super) fn coordinated_hook_branch_writer(
    administration: StoreAdministration,
) -> crate::mcp::server::HookBranchWriter {
    Arc::new(move |mut request| {
        let administration = administration.clone();
        Box::pin(async move {
            let canonical_root = request
                .root
                .canonicalize()
                .unwrap_or_else(|_| request.root.clone());
            // R4: reuse the write's single branch resolution rather than
            // re-opening the repository ahead of the direct writer's own gates.
            let active_branch = request.live_branch.resolve_for(&canonical_root);
            let mounted = administration.mounted_project_graphs().await;
            if let Some(graph) = mounted.iter().find(|graph| {
                graph.project_root() == canonical_root
                    && graph.active_branch() == active_branch.as_deref()
            }) {
                request.graph = Arc::clone(graph);
            } else if !mounted
                .iter()
                .any(|graph| Arc::ptr_eq(graph, &request.graph))
                || request.graph.branch_drifted_with(&request.live_branch)
            {
                return Err(TraceDecayError::Config {
                    message: "retained hook branch graph is unavailable".to_string(),
                });
            }
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

    let result = async {
        let project_root =
            handshake
                .project_path
                .as_deref()
                .ok_or_else(|| TraceDecayError::Config {
                    message: "branch add requires a project path".to_string(),
                })?;
        let canonical_root = project_root
            .canonicalize()
            .unwrap_or_else(|_| project_root.to_path_buf());
        let active_branch = crate::branch::current_branch(&canonical_root);
        let mounted = administration.mounted_project_graphs().await;
        let cg = mounted
            .iter()
            .find(|graph| {
                graph.project_root() == canonical_root
                    && graph.active_branch() == active_branch.as_deref()
            })
            .or_else(|| {
                mounted
                    .iter()
                    .find(|graph| graph.project_root() == canonical_root)
            })
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: "retained branch-add graph is unavailable".to_string(),
            })?;
        administration
            .with_writer(|| async { cg.track_worktree_branch(cg.project_root(), branch).await })
            .await
    }
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
