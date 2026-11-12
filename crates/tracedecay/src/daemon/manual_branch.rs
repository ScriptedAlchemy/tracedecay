//! Daemon adaptation for operator-requested branch activation.
//!
//! [`tracedecay_application::pr_tracking`] owns Git worktree ownership proofs
//! and the per-branch lifecycle lease. This adapter resolves the requested
//! branch head, prepares an exact linked worktree, and mounts it through the
//! daemon's retained code-index scheduler. Activation, failed-activation
//! cleanup, and branch retirement all run under the caller's lifecycle lease.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use tracedecay_application::pr_tracking::{
    ManualBranchActivation, ManualBranchActivationError, ManualBranchArtifactOwnershipV1,
    ManualBranchArtifactsV1, ManualBranchLifecycleLeaseV1, PrCommandControlV1 as PrCommandControl,
    cleanup_owned_worktree, cleanup_owned_worktree_off_runtime, default_pr_command_control,
    manual_branch_artifact_ownership_off_runtime, manual_branch_artifacts_match_off_runtime,
    manual_branch_source_owns_artifacts, prepare_manual_branch_worktree, resolve_branch_head,
};
#[cfg(test)]
use tracedecay_application::pr_tracking::acquire_manual_branch_lifecycle;
use tracedecay_domain::ProjectId;

use tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSchedulerRegistryV1;

const CODE_INDEX_SCHEDULER_UNAVAILABLE: &str = "code_index_scheduler_unavailable";

fn scheduler_unavailable(detail: &str) -> String {
    format!("{CODE_INDEX_SCHEDULER_UNAVAILABLE}: {detail}")
}

async fn git_authority_available(repo_root: &Path) -> bool {
    let repo = repo_root.to_path_buf();
    tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        tracedecay_runtime_core::worktree::git_worktree_root(&repo).is_some()
    })
    .await
    .ok()
    .unwrap_or(false)
}

use super::branch_admin::StoreAdministration;


#[derive(Clone, Copy)]
struct PrStoreAdministration<'a> {
    schedulers: Option<&'a CodeIndexSchedulerRegistryV1>,
    graph: Option<&'a Arc<tracedecay_project::project::TraceDecay>>,
    command_control: &'a PrCommandControl,
    stores: Option<&'a StoreAdministration>,
}

impl<'a> PrStoreAdministration<'a> {
    fn with_control(
        stores: &'a StoreAdministration,
        schedulers: &'a CodeIndexSchedulerRegistryV1,
        graph: &'a Arc<tracedecay_project::project::TraceDecay>,
        command_control: &'a PrCommandControl,
    ) -> Self {
        Self {
            schedulers: Some(schedulers),
            graph: Some(graph),
            command_control,
            stores: Some(stores),
        }
    }

    #[cfg(test)]
    fn unhosted_with_control(
        schedulers: &'a CodeIndexSchedulerRegistryV1,
        graph: &'a Arc<tracedecay_project::project::TraceDecay>,
        command_control: &'a PrCommandControl,
    ) -> Self {
        Self {
            schedulers: Some(schedulers),
            graph: Some(graph),
            command_control,
            stores: None,
        }
    }
}

/// Activates an operator-requested branch head through the same worktree
/// prep + scheduler mount path as [`track_pr`].
#[cfg(test)]
pub(crate) async fn activate_manual_branch_head(
    repo_root: &Path,
    graph: &Arc<tracedecay_project::project::TraceDecay>,
    schedulers: Option<&CodeIndexSchedulerRegistryV1>,
    branch: &str,
) -> std::result::Result<ManualBranchActivation, ManualBranchActivationError> {
    let Some(schedulers) = schedulers else {
        return Err(ManualBranchActivationError::scheduler_unavailable(
            "code-index scheduler authority is unavailable for branch activation",
        ));
    };
    let lifecycle =
        acquire_manual_branch_lifecycle(&graph.store_layout().data_root, branch).await?;
    activate_manual_branch_with_administration(
        repo_root,
        &graph.store_layout().data_root,
        branch,
        PrStoreAdministration::unhosted_with_control(
            schedulers,
            graph,
            default_pr_command_control(),
        ),
        &lifecycle,
    )
    .await
}

#[tracing::instrument(name = "daemon.manual_branch.activate", level = "trace", skip_all)]
pub(in crate::daemon) async fn activate_manual_branch_head_with_lifecycle(
    stores: &StoreAdministration,
    repo_root: &Path,
    graph: &Arc<tracedecay_project::project::TraceDecay>,
    schedulers: Option<&CodeIndexSchedulerRegistryV1>,
    branch: &str,
    lifecycle: &ManualBranchLifecycleLeaseV1,
    command_control: &PrCommandControl,
) -> std::result::Result<ManualBranchActivation, ManualBranchActivationError> {
    let administration = match schedulers {
        Some(schedulers) => {
            PrStoreAdministration::with_control(stores, schedulers, graph, command_control)
        }
        None => PrStoreAdministration {
            schedulers: None,
            graph: Some(graph),
            command_control,
            stores: Some(stores),
        },
    };
    activate_manual_branch_with_administration(
        repo_root,
        &graph.store_layout().data_root,
        branch,
        administration,
        lifecycle,
    )
    .await
}

#[tracing::instrument(
    name = "daemon.manual_branch.activate_manual_branch",
    level = "trace",
    skip_all
)]
#[expect(
    clippy::too_many_lines,
    reason = "Activation holds the lifecycle lease across resolve-checkout-index so a lease change aborts before any durable branch state is published."
)]
async fn activate_manual_branch_with_administration(
    repo_root: &Path,
    data_root: &Path,
    branch: &str,
    administration: PrStoreAdministration<'_>,
    lifecycle: &ManualBranchLifecycleLeaseV1,
) -> std::result::Result<ManualBranchActivation, ManualBranchActivationError> {
    if !lifecycle.matches_branch(branch) {
        return Err(ManualBranchActivationError::activation_failed(
            "manual branch lifecycle lease does not match requested branch",
        ));
    }
    let Some(schedulers) = administration.schedulers else {
        return Err(ManualBranchActivationError::scheduler_unavailable(
            "code-index scheduler authority is unavailable for branch activation",
        ));
    };
    let Some(graph) = administration.graph else {
        return Err(ManualBranchActivationError::scheduler_unavailable(
            "code-index scheduler authority is unavailable for branch activation",
        ));
    };
    if !git_authority_available(repo_root).await {
        return Err(ManualBranchActivationError::git_unavailable(
            "git authority is unavailable for branch activation",
        ));
    }

    if branch.starts_with('-') || branch.is_empty() {
        return Err(ManualBranchActivationError::invalid_ref(format!(
            "branch '{branch}' is not a valid branch name"
        )));
    }

    let command_control = administration.command_control.clone();
    let repo = repo_root.to_path_buf();
    let branch_name = branch.to_string();
    let head_sha = match tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        resolve_branch_head(&repo, &branch_name, &command_control)
    })
    .await
    {
        Ok(Ok(sha)) => sha,
        Ok(Err(error)) => return Err(error),
        Err(error) => {
            return Err(ManualBranchActivationError::activation_failed(format!(
                "branch resolution join error: {error}"
            )));
        }
    };

    if !lifecycle.matches_branch(branch) {
        return Err(ManualBranchActivationError::activation_failed(
            "manual branch lifecycle lease changed before activation",
        ));
    }
    let artifacts = ManualBranchArtifactsV1::for_head(data_root, branch, &head_sha);
    let worktree = artifacts.worktree.clone();
    if worktree.try_exists().map_err(|error| {
        ManualBranchActivationError::git_unavailable(format!(
            "cannot inspect manual worktree '{}': {error}",
            worktree.display()
        ))
    })? && manual_branch_artifacts_match_off_runtime(
        repo_root,
        &artifacts,
        &head_sha,
        administration.command_control.clone(),
    )
    .await?
    {
        if !schedulers.is_worktree_mounted(&worktree).await {
            activate_linked_worktree(administration.stores, schedulers, graph, &worktree)
                .await
                .map_err(ManualBranchActivationError::activation_failed)?;
        }
        return Ok(ManualBranchActivation {
            branch: branch.to_string(),
            head_sha,
            worktree,
            outcome: tracedecay_runtime_core::branch::BranchAddOutcome::AlreadyTracked,
        });
    }

    let tracking_ref = artifacts.tracking_ref.clone();
    let label = artifacts.label.clone();
    if worktree.try_exists().map_err(|error| {
        ManualBranchActivationError::git_unavailable(format!(
            "cannot inspect manual worktree '{}': {error}",
            worktree.display()
        ))
    })? {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "existing manual worktree '{}' does not match requested branch generation",
            worktree.display()
        )));
    }
    let repo = repo_root.to_path_buf();
    let wt = worktree.clone();
    let tref = tracking_ref.clone();
    let label_for_prep = label.clone();
    let expected_head = head_sha.clone();
    let command_control = administration.command_control.clone();
    match tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        prepare_manual_branch_worktree(
            &repo,
            &wt,
            &tref,
            &label_for_prep,
            &expected_head,
            &command_control,
        )
    })
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => {
            return cleanup_failed_manual_track(
                repo_root,
                &worktree,
                &tracking_ref,
                &label,
                &head_sha,
                administration,
                ManualBranchActivationError::activation_failed(reason),
            )
            .await;
        }
        Err(error) => {
            return cleanup_failed_manual_track(
                repo_root,
                &worktree,
                &tracking_ref,
                &label,
                &head_sha,
                administration,
                ManualBranchActivationError::activation_failed(format!(
                    "worktree preparation join error: {error}"
                )),
            )
            .await;
        }
    }

    match activate_linked_worktree(administration.stores, schedulers, graph, &worktree).await {
        Ok(()) => Ok(ManualBranchActivation {
            branch: branch.to_string(),
            head_sha,
            worktree,
            outcome: tracedecay_runtime_core::branch::BranchAddOutcome::Added,
        }),
        Err(reason) => {
            cleanup_failed_manual_track(
                repo_root,
                &worktree,
                &tracking_ref,
                &label,
                &head_sha,
                administration,
                ManualBranchActivationError::activation_failed(reason),
            )
            .await
        }
    }
}

async fn cleanup_failed_manual_track(
    repo_root: &Path,
    worktree: &Path,
    tracking_ref: &str,
    label: &str,
    head_sha: &str,
    administration: PrStoreAdministration<'_>,
    original: ManualBranchActivationError,
) -> std::result::Result<ManualBranchActivation, ManualBranchActivationError> {
    match retire_worktree_mount(administration.schedulers, worktree).await {
        Ok(()) => {
            let cleanup_control = PrCommandControl::default();
            if !cleanup_owned_worktree_off_runtime(
                repo_root,
                worktree,
                tracking_ref,
                label,
                head_sha,
                cleanup_control,
            )
            .await?
            {
                return Err(ManualBranchActivationError::activation_failed(format!(
                    "{original}; incomplete branch worktree ownership changed before cleanup"
                )));
            }
            Err(original)
        }
        Err(cleanup_reason) => Err(ManualBranchActivationError::activation_failed(format!(
            "{original}; failed to remove incomplete branch store: {cleanup_reason}"
        ))),
    }
}

/// Retires the exact artifacts created by a newly activated manual branch when
/// its subsequent metadata sealing fails. Callers retain the same lifecycle
/// lease that covered activation, so no concurrent add or removal can replace
/// the worktree between the ownership proof and cleanup.
#[tracing::instrument(
    name = "daemon.manual_branch.cleanup_manual_activation",
    level = "trace",
    skip_all
)]
pub(crate) async fn cleanup_manual_branch_activation(
    repo_root: &Path,
    data_root: &Path,
    schedulers: &CodeIndexSchedulerRegistryV1,
    activation: &ManualBranchActivation,
    lifecycle: &ManualBranchLifecycleLeaseV1,
) -> std::result::Result<(), ManualBranchActivationError> {
    if !lifecycle.matches_branch(&activation.branch) {
        return Err(ManualBranchActivationError::activation_failed(
            "manual branch lifecycle lease does not match failed activation",
        ));
    }
    let artifacts =
        ManualBranchArtifactsV1::for_head(data_root, &activation.branch, &activation.head_sha);
    if artifacts.worktree != activation.worktree {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "failed activation worktree '{}' does not match exact branch identity",
            activation.worktree.display()
        )));
    }
    retire_worktree_mount(Some(schedulers), &artifacts.worktree)
        .await
        .map_err(ManualBranchActivationError::activation_failed)?;
    if !cleanup_owned_worktree_off_runtime(
        repo_root,
        &artifacts.worktree,
        &artifacts.tracking_ref,
        &artifacts.label,
        &activation.head_sha,
        default_pr_command_control().clone(),
    )
    .await?
    {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "failed activation for branch '{}' changed before exact cleanup",
            activation.branch
        )));
    }
    Ok(())
}

/// Retires only the manual artifacts proven by a persisted graph-source entry
/// before branch metadata removal commits. The source's exact worktree,
/// synthetic ref, and OID are the ownership proof; a legacy entry without
/// that proof is intentionally left untouched rather than guessing at Git
/// artifacts. The lifecycle lease returns only after synchronous Git teardown
/// finishes, so request cancellation cannot admit a concurrent replacement
/// while the blocking worker still owns those artifacts.
#[tracing::instrument(
    name = "daemon.manual_branch.cleanup_manual_retirement",
    level = "trace",
    skip_all
)]
pub(crate) async fn cleanup_manual_branch_retirement(
    repo_root: &Path,
    data_root: &Path,
    schedulers: &CodeIndexSchedulerRegistryV1,
    branch: &str,
    source: &tracedecay_runtime_core::branch_meta::BranchGraphSourceV1,
    lifecycle: ManualBranchLifecycleLeaseV1,
) -> std::result::Result<ManualBranchLifecycleLeaseV1, ManualBranchActivationError> {
    if !lifecycle.matches_branch(branch) {
        return Err(ManualBranchActivationError::activation_failed(
            "manual branch lifecycle lease does not match metadata retirement",
        ));
    }
    if !manual_branch_source_owns_artifacts(data_root, branch, source) {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "stored branch provenance does not own manual artifacts for '{branch}'"
        )));
    }
    let artifacts = ManualBranchArtifactsV1::for_head(data_root, branch, &source.source_oid);
    let expected_worktree = artifacts
        .worktree
        .canonicalize()
        .unwrap_or(artifacts.worktree.clone());
    let ownership = manual_branch_artifact_ownership_off_runtime(
        repo_root,
        &expected_worktree,
        &artifacts.tracking_ref,
        &artifacts.label,
        &source.source_oid,
        default_pr_command_control().clone(),
    )
    .await?;
    if ownership == ManualBranchArtifactOwnershipV1::Foreign {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "manual artifacts for branch '{branch}' are no longer owned by the stored source"
        )));
    }
    retire_worktree_mount(Some(schedulers), &expected_worktree)
        .await
        .map_err(ManualBranchActivationError::activation_failed)?;
    let repo_root = repo_root.to_path_buf();
    let tracking_ref = artifacts.tracking_ref;
    let label = artifacts.label;
    let source_oid = source.source_oid.clone();
    let (cleaned, lifecycle) = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        let cleaned = cleanup_owned_worktree(
            &repo_root,
            &expected_worktree,
            &tracking_ref,
            &label,
            &source_oid,
            default_pr_command_control(),
        );
        (cleaned, lifecycle)
    })
    .await
    .map_err(|error| {
        ManualBranchActivationError::activation_failed(format!(
            "manual branch retirement cleanup task did not complete: {error}"
        ))
    })?;
    if !cleaned? {
        return Err(ManualBranchActivationError::activation_failed(format!(
            "manual artifacts for branch '{branch}' changed before exact retirement"
        )));
    }
    Ok(lifecycle)
}

pub(crate) async fn retire_worktree_mount(
    schedulers: Option<&CodeIndexSchedulerRegistryV1>,
    worktree: &Path,
) -> std::result::Result<(), String> {
    let Some(schedulers) = schedulers else {
        return Err(scheduler_unavailable(
            "code-index scheduler authority is unavailable for worktree retirement",
        ));
    };
    let root = worktree
        .canonicalize()
        .unwrap_or_else(|_| worktree.to_path_buf());
    let roots = BTreeSet::from([root]);
    if !schedulers.retire_project_roots(&roots).await {
        return Err(scheduler_unavailable(
            "code-index scheduler did not finish worktree retirement",
        ));
    }
    Ok(())
}

#[tracing::instrument(
    name = "daemon.manual_branch.activate_worktree",
    level = "trace",
    skip_all
)]
async fn activate_linked_worktree(
    stores: Option<&StoreAdministration>,
    schedulers: &CodeIndexSchedulerRegistryV1,
    graph: &tracedecay_project::project::TraceDecay,
    worktree: &Path,
) -> std::result::Result<(), String> {
    let project_id = graph
        .store_layout()
        .identity
        .project_id
        .as_deref()
        .ok_or_else(|| {
            scheduler_unavailable("project identity is unavailable for worktree activation")
        })?;
    let project_id = ProjectId::new(project_id.to_owned()).map_err(|error| {
        scheduler_unavailable(&format!(
            "invalid project identity for worktree activation: {error}"
        ))
    })?;
    let store_root = graph.store_layout().data_root.join("code-index-v1");
    let graph_runtime = graph.retained_store_runtime_registry();
    let project_database = Arc::new(graph.db().clone());
    let pending_source = match stores {
        Some(stores) => {
            let scope =
                tracedecay_code_index_runtime::resolved_scope_for_project(worktree, &project_id)
                    .map_err(|error| scheduler_unavailable(&error.to_string()))?;
            let reader = super::project_composition::project_hook_source_pending(
                stores, graph, worktree, &scope,
            )
            .await
            .map_err(|error| scheduler_unavailable(&error.to_string()))?;
            Some(reader)
        }
        None => {
            #[cfg(test)]
            {
                None
            }
            #[cfg(not(test))]
            {
                return Err(scheduler_unavailable(
                    "host admission authority is unavailable for worktree activation",
                ));
            }
        }
    };
    schedulers
        .mount_worktree_with_graph_runtime(
            project_id,
            worktree,
            store_root,
            graph_runtime.code_graph_seat_port(),
            project_database,
            tracedecay_code_index_runtime::code_index_scheduler::CodeGraphActivationPolicyV1::from_enabled(
                graph.get_config().native_graph_activation,
            ),
            tracedecay_code_index_runtime::code_index_scheduler::CodeIndexSourceInputsV1 {
                path_policy: graph.get_config().index_paths.clone(),
                pending_admission: pending_source,
            },
        )
        .await
        .map(|_| ())
        .map_err(|error| {
            scheduler_unavailable(&format!(
                "code-index scheduler rejected worktree activation: {error}"
            ))
        })
}

/// Retires the scheduler mount for a managed PR worktree. Git artifacts stay
/// intact until [`untrack_pr`] or sweep cleanup runs after this returns Ok.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests;
