use std::path::Path;
use std::sync::Arc;

use tracedecay_application::pr_tracking::{
    ManualBranchLifecycleLeaseV1, PrCommandControlV1, manual_branch_source_owns_artifacts,
    try_acquire_manual_branch_lifecycle,
};

use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexSchedulerRegistryV1, branch_publication::BranchPublicationContextV1,
};
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse};
use tracedecay_runtime_core::branch::{
    BranchAddOutcome, BranchTrackingPreparation, PreparedBranchRollbackOutcome,
    prepare_branch_tracking_in_layout, rollback_prepared_branch_tracking,
};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_runtime_core::logging::log_daemon_event;

use super::{DaemonHandshake, StoreAdministration};

const BRANCH_ADD_TOOL_NAME: &str = "tracedecay_admin_branch_add";
const CODE_INDEX_SCHEDULER_UNAVAILABLE: &str = "code_index_scheduler_unavailable";
const PROJECT_PATH_UNAVAILABLE: &str = "project_path_unavailable";
const BRANCH_TRACKING_FAILED: &str = "branch_tracking_failed";

pub(super) struct BranchAddRequest {
    pub(super) id: serde_json::Value,
    branch: std::result::Result<String, String>,
}

pub(super) fn parse_branch_add_request(
    request: Option<&JsonRpcRequest>,
) -> Option<BranchAddRequest> {
    let request = request?;
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
        id: request.id.clone().unwrap_or(serde_json::Value::Null),
        branch,
    })
}

#[hotpath::measure(label = "daemon.branch_add.response", future = true)]
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
    let Some(schedulers) = schedulers else {
        return typed_project_route_error(
            request.id.clone(),
            CODE_INDEX_SCHEDULER_UNAVAILABLE,
            true,
            "code-index scheduler authority is unavailable for branch activation",
        );
    };
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
        match activate_and_track_manual_branch(
            administration,
            &canonical_root,
            &graph,
            schedulers,
            branch,
        )
        .await
        {
            Ok(activation) => {
                JsonRpcResponse::success(request.id.clone(), branch_add_tool_result(&activation))
            }
            Err(error) => typed_tracking_error(request.id.clone(), &error),
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

/// Production branch-add journey: activate the requested linked worktree,
/// then ask the code-index runtime to seal its exact generation and provenance.
#[cfg(unix)]
#[hotpath::measure(label = "daemon.branch_add.activate_and_track", future = true)]
async fn activate_and_track_manual_branch(
    administration: &StoreAdministration,
    project_root: &Path,
    graph: &Arc<crate::tracedecay::TraceDecay>,
    schedulers: &CodeIndexSchedulerRegistryV1,
    branch: &str,
) -> Result<BranchAddOutcome, TraceDecayError> {
    let data_root = graph.store_layout().data_root.clone();
    let project_root = project_root.to_path_buf();
    let graph = Arc::clone(graph);
    let schedulers = schedulers.clone();
    let branch = branch.to_owned();

    administration
        .admit_manual_branch_publication(|cancellation, admitted| async move {
            let result = async {
                let lifecycle =
                    try_acquire_manual_branch_lifecycle(&data_root, &branch).map_err(|error| {
                        TraceDecayError::project_route(
                            error.reason_code(),
                            error.retryable(),
                            error.detail(),
                        )
                    })?;
                let prepared = match prepare_branch_tracking_in_layout(
                    &project_root,
                    &branch,
                    &data_root,
                )
                .await
                .map_err(|error| {
                    TraceDecayError::project_route(
                        BRANCH_TRACKING_FAILED,
                        false,
                        format!("failed to prepare branch tracking for '{branch}': {error}"),
                    )
                })? {
                    BranchTrackingPreparation::Added(prepared) => Some(prepared),
                    BranchTrackingPreparation::AlreadyTracked => None,
                    BranchTrackingPreparation::Deferred => {
                        let _ = admitted.send(());
                        return Ok(BranchAddOutcome::Deferred);
                    }
                };
                let _ = admitted.send(());
                let tracked = activate_and_track_manual_branch_owned(
                    project_root,
                    graph,
                    schedulers,
                    branch.clone(),
                    data_root.clone(),
                    lifecycle,
                    cancellation,
                )
                .await;
                if let (Err(error), Some(prepared)) = (&tracked, prepared.as_deref()) {
                    match rollback_prepared_branch_tracking(&data_root, prepared).map_err(
                        |rollback| {
                            TraceDecayError::project_route(
                                BRANCH_TRACKING_FAILED,
                                true,
                                format!(
                                    "branch activation failed: {error}; branch rollback failed: {rollback}"
                                ),
                            )
                        },
                    )? {
                        PreparedBranchRollbackOutcome::RolledBack
                        | PreparedBranchRollbackOutcome::NoMatch => {}
                    }
                }
                tracked
            }
            .await;
            match &result {
                Ok(outcome) => log_daemon_event(
                    "manual_branch_publication",
                    &[
                        ("branch", branch.clone()),
                        ("outcome", branch_add_outcome_name(outcome).to_owned()),
                    ],
                ),
                Err(error) => log_daemon_event(
                    "manual_branch_publication",
                    &[
                        ("branch", branch.clone()),
                        ("outcome", "failed".to_owned()),
                        ("reason", error.to_string()),
                    ],
                ),
            }
            result
        })
        .await
}

#[cfg(unix)]
#[hotpath::measure(label = "daemon.branch_add.owner", future = true)]
pub(super) async fn activate_and_track_manual_branch_owned(
    project_root: std::path::PathBuf,
    graph: Arc<crate::tracedecay::TraceDecay>,
    schedulers: CodeIndexSchedulerRegistryV1,
    branch: String,
    data_root: std::path::PathBuf,
    lifecycle: ManualBranchLifecycleLeaseV1,
    cancellation: CancellationToken,
) -> Result<BranchAddOutcome, TraceDecayError> {
    let publication = branch_publication_context(&graph)?;
    let activation = super::pr_autotrack::activate_manual_branch_head_with_lifecycle(
        &project_root,
        &graph,
        Some(&schedulers),
        &branch,
        &lifecycle,
        &PrCommandControlV1::with_cancellation(cancellation.clone()),
    )
    .await
    .map_err(|error| {
        TraceDecayError::project_route(error.reason_code(), error.retryable(), error.detail())
    })?;
    if !lifecycle.matches_branch(&branch) {
        return Err(TraceDecayError::project_route(
            BRANCH_TRACKING_FAILED,
            true,
            "manual branch lifecycle lease does not match branch sealing request",
        ));
    }
    let previous_source = tracedecay_runtime_core::branch_meta::load_branch_meta(&data_root)
        .and_then(|meta| {
            meta.branches
                .get(&branch)
                .and_then(|entry| entry.graph_source.clone())
        });
    let tracked = publication
        .track_exact_worktree_branch(
            &schedulers,
            &project_root,
            &activation.worktree,
            &branch,
            &cancellation,
        )
        .await;
    match tracked {
        Ok(outcome) => {
            if outcome != BranchAddOutcome::Deferred
                && let Some(previous) = previous_source
                && previous.source_oid != activation.head_sha
                && previous.worktree_root != activation.worktree.to_string_lossy()
                && manual_branch_source_owns_artifacts(&data_root, &branch, &previous)
            {
                super::pr_autotrack::cleanup_manual_branch_retirement(
                    &project_root, &data_root, &schedulers, &branch, &previous, lifecycle,
                ).await.map_err(|error| TraceDecayError::project_route(
                    error.reason_code(), error.retryable(),
                    format!("branch publication committed; prior generation retirement failed: {error}"),
                ))?;
            }
            Ok(outcome)
        }
        Err(error) if activation.outcome == BranchAddOutcome::Added => {
            super::pr_autotrack::cleanup_manual_branch_activation(
                &project_root,
                &data_root,
                &schedulers,
                &activation,
                &lifecycle,
            )
            .await
            .map_err(|cleanup| {
                TraceDecayError::project_route(
                    cleanup.reason_code(),
                    cleanup.retryable(),
                    format!(
                        "branch sealing failed: {error}; exact activation cleanup failed: {cleanup}"
                    ),
                )
            })?;
            Err(error)
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn branch_publication_context(
    graph: &crate::tracedecay::TraceDecay,
) -> Result<BranchPublicationContextV1, TraceDecayError> {
    BranchPublicationContextV1::new(
        graph.store_layout().identity.project_id.as_deref(),
        graph.project_root(),
        &graph.store_layout().data_root,
    )
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

pub(super) fn typed_project_route_error(
    id: serde_json::Value,
    reason_code: &str,
    retryable: bool,
    detail: &str,
) -> JsonRpcResponse {
    let error = TraceDecayError::project_route(reason_code, retryable, detail);
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

fn typed_tracking_error(id: serde_json::Value, error: &TraceDecayError) -> JsonRpcResponse {
    if let Some((reason_code, retryable, detail)) = error.project_route_context() {
        return typed_project_route_error(id, reason_code, retryable, detail);
    }
    typed_project_route_error(id, BRANCH_TRACKING_FAILED, true, &error.to_string())
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
