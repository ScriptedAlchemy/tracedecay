use crate::branch::BranchAddOutcome;
use crate::errors::TraceDecayError;
use crate::mcp::{ErrorCode, JsonRpcRequest, JsonRpcResponse};

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;

use super::code_index_scheduler::{
    CodeIndexSchedulerRegistryV1, ServingGenerationInstallationOutcomeV1,
    ServingGenerationRollbackOutcomeV1,
};
use super::{DaemonHandshake, StoreAdministration};

const BRANCH_ADD_TOOL_NAME: &str = "tracedecay_admin_branch_add";
const CODE_INDEX_SCHEDULER_UNAVAILABLE: &str = "code_index_scheduler_unavailable";
const PROJECT_PATH_UNAVAILABLE: &str = "project_path_unavailable";
const CODE_INDEX_ACTIVATION_UNAVAILABLE: &str = "code_index_activation_unavailable";
const CODE_INDEX_IDENTITY_MISMATCH: &str = "code_index_scheduler_identity_mismatch";
const GIT_SNAPSHOT_UNAVAILABLE: &str = "git_snapshot_unavailable";
const BRANCH_TRACKING_FAILED: &str = "branch_tracking_failed";

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
        match activate_and_track_manual_branch(&canonical_root, &graph, schedulers, branch).await {
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
/// then seal the exact scheduler generation and its Git provenance into the
/// canonical project-store branch metadata.
#[cfg(unix)]
async fn activate_and_track_manual_branch(
    project_root: &Path,
    graph: &Arc<crate::tracedecay::TraceDecay>,
    schedulers: &CodeIndexSchedulerRegistryV1,
    branch: &str,
) -> Result<BranchAddOutcome, TraceDecayError> {
    let data_root = graph.store_layout().data_root.clone();
    let lifecycle = super::pr_autotrack::try_acquire_manual_branch_lifecycle(&data_root, branch)
        .map_err(|error| {
            TraceDecayError::project_route(error.reason_code(), error.retryable(), error.detail())
        })?;
    let project_root = project_root.to_path_buf();
    let graph = Arc::clone(graph);
    let schedulers = schedulers.clone();
    let branch = branch.to_owned();

    // This operation owns the exact branch lifecycle lease through Git
    // replacement, scheduler mount, metadata sealing, and rollback. A host
    // request may be cancelled, but its bounded owner must finish before a
    // retry can observe or replace this branch's artifacts.
    tokio::spawn(async move {
        activate_and_track_manual_branch_owned(
            project_root,
            graph,
            schedulers,
            branch,
            data_root,
            lifecycle,
        )
        .await
    })
    .await
    .map_err(|error| {
        TraceDecayError::project_route(
            BRANCH_TRACKING_FAILED,
            true,
            format!("manual branch lifecycle owner stopped before completion: {error}"),
        )
    })?
}

#[cfg(unix)]
async fn activate_and_track_manual_branch_owned(
    project_root: std::path::PathBuf,
    graph: Arc<crate::tracedecay::TraceDecay>,
    schedulers: CodeIndexSchedulerRegistryV1,
    branch: String,
    data_root: std::path::PathBuf,
    lifecycle: super::pr_autotrack::ManualBranchLifecycleLeaseV1,
) -> Result<BranchAddOutcome, TraceDecayError> {
    let activation = super::pr_autotrack::activate_manual_branch_head_with_lifecycle(
        &project_root,
        &graph,
        Some(&schedulers),
        &branch,
        &lifecycle,
    )
    .await
    .map_err(|error| {
        TraceDecayError::project_route(error.reason_code(), error.retryable(), error.detail())
    })?;
    let tracked = track_exact_worktree_branch_with_lifecycle(
        &graph,
        &schedulers,
        &project_root,
        &activation.worktree,
        &branch,
        &lifecycle,
    )
    .await;
    match tracked {
        Ok(outcome) => Ok(outcome),
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

/// Seals the current, exact Git snapshot for one mounted branch worktree.
///
/// This wrapper is the in-process production composition harness's entry to
/// the shared `track_exact_worktree_branch_with_lifecycle` authority. It
/// intentionally captures one Git snapshot, requests a scheduler refresh,
/// then requires exact repository/worktree/ref/OID equality before
/// publishing metadata.
#[cfg(any(test, feature = "test-transport"))]
pub(crate) async fn track_exact_worktree_branch(
    graph: &Arc<crate::tracedecay::TraceDecay>,
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    worktree_root: &Path,
    branch: &str,
) -> Result<BranchAddOutcome, TraceDecayError> {
    let lifecycle = super::pr_autotrack::try_acquire_manual_branch_lifecycle(
        &graph.store_layout().data_root,
        branch,
    )
    .map_err(|error| {
        TraceDecayError::project_route(error.reason_code(), error.retryable(), error.detail())
    })?;
    track_exact_worktree_branch_with_lifecycle(
        graph,
        schedulers,
        project_root,
        worktree_root,
        branch,
        &lifecycle,
    )
    .await
}

async fn track_exact_worktree_branch_with_lifecycle(
    graph: &Arc<crate::tracedecay::TraceDecay>,
    schedulers: &CodeIndexSchedulerRegistryV1,
    project_root: &Path,
    worktree_root: &Path,
    branch: &str,
    lifecycle: &super::pr_autotrack::ManualBranchLifecycleLeaseV1,
) -> Result<BranchAddOutcome, TraceDecayError> {
    if !lifecycle.matches_branch(branch) {
        return Err(TraceDecayError::project_route(
            BRANCH_TRACKING_FAILED,
            true,
            "manual branch lifecycle lease does not match branch sealing request",
        ));
    }
    let canonical_project_root = project_root.canonicalize().map_err(|error| {
        TraceDecayError::project_route(
            CODE_INDEX_IDENTITY_MISMATCH,
            false,
            format!(
                "failed to canonicalize branch project root '{}': {error}",
                project_root.display()
            ),
        )
    })?;
    if !graph_matches_project(graph, &canonical_project_root) {
        return Err(TraceDecayError::project_route(
            CODE_INDEX_IDENTITY_MISMATCH,
            false,
            format!(
                "branch project root '{}' is not owned by the retained project graph",
                canonical_project_root.display()
            ),
        ));
    }
    let canonical_worktree_root = worktree_root.canonicalize().map_err(|error| {
        TraceDecayError::project_route(
            CODE_INDEX_IDENTITY_MISMATCH,
            false,
            format!(
                "failed to canonicalize branch worktree '{}': {error}",
                worktree_root.display()
            ),
        )
    })?;
    let source_branch =
        crate::branch::current_branch(&canonical_worktree_root).ok_or_else(|| {
            TraceDecayError::project_route(
                GIT_SNAPSHOT_UNAVAILABLE,
                false,
                format!(
                    "branch graph publication requires an attached source branch for '{}'",
                    canonical_worktree_root.display()
                ),
            )
        })?;
    let source = capture_exact_branch_source(
        graph,
        schedulers,
        &canonical_project_root,
        &canonical_worktree_root,
        &source_branch,
    )
    .await?;
    let data_root = graph.store_layout().data_root.clone();
    let prepared = match crate::branch::prepare_branch_tracking_in_layout(
        &canonical_worktree_root,
        branch,
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
        crate::branch::BranchTrackingPreparation::Added(prepared) => Some(prepared),
        crate::branch::BranchTrackingPreparation::AlreadyTracked => None,
        crate::branch::BranchTrackingPreparation::Deferred => {
            return Ok(BranchAddOutcome::Deferred);
        }
    };
    let expected_source = crate::branch_meta::load_branch_meta(&data_root).and_then(|meta| {
        meta.branches
            .get(branch)
            .and_then(|entry| entry.graph_source.clone())
    });
    let generation =
        match await_exact_branch_generation(schedulers, &canonical_worktree_root, &source).await {
            Ok(generation) => generation,
            Err(error) => {
                rollback_failed_branch_tracking(&data_root, prepared.as_deref(), None, &error)
                    .await?;
                return Err(error);
            }
        };
    let ServingGenerationInstallationOutcomeV1::Installed(installation) = schedulers
        .install_exact_serving_generation(&canonical_worktree_root, &generation)
        .await
    else {
        let error = TraceDecayError::project_route(
            CODE_INDEX_ACTIVATION_UNAVAILABLE,
            true,
            format!(
                "exact branch generation was replaced before publication for '{}'",
                canonical_worktree_root.display()
            ),
        );
        rollback_failed_branch_tracking(&data_root, prepared.as_deref(), None, &error).await?;
        return Err(error);
    };
    let publication = crate::branch_meta::publish_graph_source(
        &data_root,
        branch,
        expected_source.as_ref(),
        source.clone(),
    )
    .map_err(|error| {
        TraceDecayError::project_route(
            BRANCH_TRACKING_FAILED,
            true,
            format!("failed to publish branch source for '{branch}': {error}"),
        )
    });
    match publication {
        Ok(crate::branch_meta::BranchGraphSourcePublishOutcomeV1::Published(publication)) => {
            match schedulers
                .commit_serving_generation_installation(&canonical_worktree_root, installation)
                .await
            {
                ServingGenerationRollbackOutcomeV1::Cleared => Ok(BranchAddOutcome::Added),
                ServingGenerationRollbackOutcomeV1::NoMatch => {
                    let error = TraceDecayError::project_route(
                        CODE_INDEX_ACTIVATION_UNAVAILABLE,
                        true,
                        format!("serving generation changed while publishing branch '{branch}'"),
                    );
                    rollback_failed_branch_tracking(
                        &data_root,
                        prepared.as_deref(),
                        Some(&publication),
                        &error,
                    )
                    .await?;
                    Err(error)
                }
            }
        }
        Ok(crate::branch_meta::BranchGraphSourcePublishOutcomeV1::AlreadyPublished(_)) => {
            match schedulers
                .commit_serving_generation_installation(&canonical_worktree_root, installation)
                .await
            {
                ServingGenerationRollbackOutcomeV1::Cleared => Ok(BranchAddOutcome::AlreadyTracked),
                ServingGenerationRollbackOutcomeV1::NoMatch => Err(TraceDecayError::project_route(
                    CODE_INDEX_ACTIVATION_UNAVAILABLE,
                    true,
                    format!(
                        "serving generation changed before exact branch replay completed for '{branch}'"
                    ),
                )),
            }
        }
        Ok(crate::branch_meta::BranchGraphSourcePublishOutcomeV1::CompareAndSwapMiss {
            observed: Some(observed),
        }) if observed.matches_draft(&source) => match schedulers
            .commit_serving_generation_installation(&canonical_worktree_root, installation)
            .await
        {
            ServingGenerationRollbackOutcomeV1::Cleared => Ok(BranchAddOutcome::AlreadyTracked),
            ServingGenerationRollbackOutcomeV1::NoMatch => Err(TraceDecayError::project_route(
                CODE_INDEX_ACTIVATION_UNAVAILABLE,
                true,
                format!(
                    "serving generation changed before exact branch replay completed for '{branch}'"
                ),
            )),
        },
        Ok(outcome) => {
            let error = TraceDecayError::project_route(
                BRANCH_TRACKING_FAILED,
                true,
                format!(
                    "branch source publication did not commit exact provenance for '{branch}': {outcome:?}"
                ),
            );
            let _ = schedulers
                .commit_serving_generation_installation(&canonical_worktree_root, installation)
                .await;
            rollback_failed_branch_tracking(&data_root, prepared.as_deref(), None, &error).await?;
            Err(error)
        }
        Err(error) => {
            let _ = schedulers
                .commit_serving_generation_installation(&canonical_worktree_root, installation)
                .await;
            rollback_failed_branch_tracking(&data_root, prepared.as_deref(), None, &error).await?;
            Err(error)
        }
    }
}

pub(crate) async fn capture_exact_branch_source(
    graph: &Arc<crate::tracedecay::TraceDecay>,
    schedulers: &CodeIndexSchedulerRegistryV1,
    canonical_project_root: &Path,
    canonical_worktree_root: &Path,
    branch: &str,
) -> Result<crate::branch_meta::BranchGraphSourceDraftV1, TraceDecayError> {
    let project_id = graph
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| {
            TraceDecayError::project_route(
                CODE_INDEX_IDENTITY_MISMATCH,
                false,
                "branch graph publication requires an authoritative project identity",
            )
        })?;
    let scope = schedulers
        .serving_code_scope(canonical_worktree_root)
        .await
        .ok_or_else(|| {
            TraceDecayError::project_route(
                CODE_INDEX_SCHEDULER_UNAVAILABLE,
                true,
                format!(
                    "code-index scheduler authority is unavailable for branch worktree '{}' in project '{}'",
                    canonical_worktree_root.display(),
                    canonical_project_root.display()
                ),
            )
        })?;
    if scope
        .shutting_down
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(TraceDecayError::project_route(
            CODE_INDEX_SCHEDULER_UNAVAILABLE,
            true,
            format!(
                "code-index scheduler is shutting down for branch worktree '{}'",
                canonical_worktree_root.display()
            ),
        ));
    }
    let project_identity = tracedecay_domain::ProjectId::new(project_id.to_owned()).map_err(
        |error| {
            TraceDecayError::project_route(
                CODE_INDEX_IDENTITY_MISMATCH,
                false,
                format!("branch graph publication has an invalid project identity '{project_id}': {error}"),
            )
        },
    )?;
    let snapshot = super::git_transactions::capture_exact_snapshot(
        canonical_worktree_root,
        project_identity.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        tracedecay_application::now_micros(),
    )
    .map_err(|error| {
        TraceDecayError::project_route(
            GIT_SNAPSHOT_UNAVAILABLE,
            true,
            format!(
                "failed to capture exact Git snapshot for branch worktree '{}': {error}",
                canonical_worktree_root.display()
            ),
        )
    })?;
    if snapshot.project_id != project_identity
        || snapshot.repository_id != scope.repository_id
        || snapshot.worktree_id.as_ref() != Some(&scope.worktree_id)
    {
        return Err(TraceDecayError::project_route(
            CODE_INDEX_IDENTITY_MISMATCH,
            false,
            format!(
                "exact Git snapshot does not match the mounted scheduler route for '{}'",
                canonical_worktree_root.display()
            ),
        ));
    }
    let (snapshot_branch, source_oid) = match snapshot.head {
        tracedecay_domain::GitHeadStateV1::Attached { branch, commit } => {
            (branch, commit.as_str().to_owned())
        }
        tracedecay_domain::GitHeadStateV1::Detached { .. }
        | tracedecay_domain::GitHeadStateV1::Unborn { .. } => {
            return Err(TraceDecayError::project_route(
                GIT_SNAPSHOT_UNAVAILABLE,
                true,
                format!(
                    "branch graph publication requires an attached committed head for '{}'",
                    canonical_worktree_root.display()
                ),
            ));
        }
    };
    let expected_reference = format!("refs/heads/{branch}");
    if snapshot_branch != expected_reference {
        return Err(TraceDecayError::project_route(
            CODE_INDEX_IDENTITY_MISMATCH,
            false,
            format!(
                "exact Git snapshot is attached to branch '{snapshot_branch}', not requested branch '{expected_reference}'"
            ),
        ));
    }
    Ok(crate::branch_meta::BranchGraphSourceDraftV1 {
        project_id: project_id.to_owned(),
        repository_id: scope.repository_id.as_str().to_owned(),
        worktree_id: scope.worktree_id.as_str().to_owned(),
        worktree_root: canonical_worktree_root.to_string_lossy().into_owned(),
        reference: snapshot_branch,
        source_oid,
    })
}

pub(crate) async fn await_exact_branch_generation(
    schedulers: &CodeIndexSchedulerRegistryV1,
    canonical_worktree_root: &Path,
    source: &crate::branch_meta::BranchGraphSourceDraftV1,
) -> Result<Arc<crate::code_index::production::CodeIndexPublishedGenerationV1>, TraceDecayError> {
    let mut publications = schedulers.subscribe_generation_publications();
    if !schedulers
        .notify_hook_overflow(canonical_worktree_root)
        .await
    {
        return Err(TraceDecayError::project_route(
            CODE_INDEX_SCHEDULER_UNAVAILABLE,
            true,
            format!(
                "code-index scheduler rejected refresh for branch worktree '{}'",
                canonical_worktree_root.display()
            ),
        ));
    }
    timeout(Duration::from_secs(20), async {
        loop {
            let scope = schedulers
                .serving_code_scope(canonical_worktree_root)
                .await
                .ok_or_else(|| {
                    TraceDecayError::project_route(
                        CODE_INDEX_SCHEDULER_UNAVAILABLE,
                        true,
                        format!(
                            "code-index scheduler disappeared for branch worktree '{}'",
                            canonical_worktree_root.display()
                        ),
                    )
                })?;
            if scope
                .shutting_down
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Err(TraceDecayError::project_route(
                    CODE_INDEX_SCHEDULER_UNAVAILABLE,
                    true,
                    format!(
                        "code-index scheduler is shutting down for branch worktree '{}'",
                        canonical_worktree_root.display()
                    ),
                ));
            }
            if let Some(generation) = scope
                .serving_generation
                .filter(|generation| generation_matches_branch_source(generation, source))
            {
                return Ok(generation);
            }
            match publications.recv().await {
                Ok(event) if event.project_root == canonical_worktree_root => {}
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err(TraceDecayError::project_route(
                        CODE_INDEX_ACTIVATION_UNAVAILABLE,
                        true,
                        format!(
                            "code-index publication stream closed for branch worktree '{}'",
                            canonical_worktree_root.display()
                        ),
                    ));
                }
            }
        }
    })
    .await
    .map_err(|_| {
        TraceDecayError::project_route(
            CODE_INDEX_ACTIVATION_UNAVAILABLE,
            true,
            format!(
                "code-index scheduler did not publish exact branch source '{}' at '{}' for '{}'",
                source.reference,
                source.source_oid,
                canonical_worktree_root.display()
            ),
        )
    })?
}

fn generation_matches_branch_source(
    generation: &crate::code_index::production::CodeIndexPublishedGenerationV1,
    source: &crate::branch_meta::BranchGraphSourceDraftV1,
) -> bool {
    let snapshot = generation.snapshot();
    generation.manifest().project_id.as_str() == source.project_id
        && snapshot.repository.as_str() == source.repository_id
        && snapshot
            .worktree
            .as_ref()
            .map(tracedecay_domain::WorktreeId::as_str)
            == Some(source.worktree_id.as_str())
        && snapshot
            .reference
            .as_ref()
            .map(tracedecay_domain::RefId::as_str)
            == Some(source.reference.as_str())
        && snapshot
            .source_revision
            .as_ref()
            .map(tracedecay_domain::CommitId::as_str)
            == Some(source.source_oid.as_str())
}

async fn rollback_failed_branch_tracking(
    data_root: &Path,
    prepared: Option<&crate::branch::PreparedBranchTracking>,
    publication: Option<&crate::branch_meta::BranchGraphSourcePublicationV1>,
    cause: &TraceDecayError,
) -> Result<(), TraceDecayError> {
    let publication_rolled_back = match publication {
        Some(publication) => {
            match crate::branch_meta::rollback_graph_source_publication(data_root, publication)
                .map_err(|error| {
                    TraceDecayError::project_route(
                        BRANCH_TRACKING_FAILED,
                        true,
                        format!(
                            "branch publication failed: {cause}; source rollback failed: {error}"
                        ),
                    )
                })? {
                crate::branch_meta::BranchGraphSourceRollbackOutcomeV1::Restored => true,
                crate::branch_meta::BranchGraphSourceRollbackOutcomeV1::NoMatch => false,
            }
        }
        None => true,
    };
    if !publication_rolled_back {
        return Ok(());
    }
    let prepared_rolled_back = match prepared {
        Some(prepared) => match crate::branch::rollback_prepared_branch_tracking(
            data_root, prepared,
        )
        .map_err(|error| {
            TraceDecayError::project_route(
                BRANCH_TRACKING_FAILED,
                true,
                format!("branch publication failed: {cause}; branch rollback failed: {error}"),
            )
        })? {
            crate::branch::PreparedBranchRollbackOutcome::RolledBack => true,
            crate::branch::PreparedBranchRollbackOutcome::NoMatch => false,
        },
        None => true,
    };
    let _ = (publication, prepared_rolled_back);
    Ok(())
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
