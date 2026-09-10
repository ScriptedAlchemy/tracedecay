//! Exact branch-generation publication through the retained code-index owner.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::time::Instant;
use tracedecay_dashboard_api::code_index_freshness_api::{
    CodeGraphServingReadinessV1, CodeIndexWorktreeFreshnessV1,
};
use tracedecay_domain::ProjectId;
use tracedecay_domain::errors::TraceDecayError;
use tracedecay_runtime_core::branch::{
    BranchAddOutcome, BranchTrackingPreparation, PreparedBranchRollbackOutcome,
};
use tracedecay_runtime_core::branch_meta::{
    BranchGraphSourceDraftV1, BranchGraphSourcePublicationV1, BranchGraphSourcePublishOutcomeV1,
    BranchGraphSourceRollbackOutcomeV1,
};
use tracedecay_runtime_core::cancellation::CancellationToken;

use super::registry::{CodeIndexServingScopeV1, ServingGenerationInstallationV1};
use super::{
    CodeIndexPublishedGenerationV1, CodeIndexSchedulerRegistryV1,
    ServingGenerationInstallationOutcomeV1, ServingGenerationRollbackOutcomeV1,
};

const CODE_INDEX_SCHEDULER_UNAVAILABLE: &str = "code_index_scheduler_unavailable";
const CODE_INDEX_ACTIVATION_UNAVAILABLE: &str = "code_index_activation_unavailable";
const CODE_INDEX_IDENTITY_MISMATCH: &str = "code_index_scheduler_identity_mismatch";
const GIT_SNAPSHOT_UNAVAILABLE: &str = "git_snapshot_unavailable";
const BRANCH_TRACKING_FAILED: &str = "branch_tracking_failed";
const BRANCH_GENERATION_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
const BRANCH_GENERATION_HARD_TIMEOUT: Duration = Duration::from_mins(30);

fn branch_publication_cancelled_error(branch: &str) -> TraceDecayError {
    TraceDecayError::project_route(
        BRANCH_TRACKING_FAILED,
        true,
        format!("branch publication was cancelled for '{branch}'"),
    )
}

/// Immutable project identity and layout required to publish branch metadata.
#[derive(Clone, Debug)]
pub struct BranchPublicationContextV1 {
    project_id: ProjectId,
    project_root: PathBuf,
    data_root: PathBuf,
}

impl BranchPublicationContextV1 {
    pub fn new(
        project_id: Option<&str>,
        project_root: &Path,
        data_root: &Path,
    ) -> Result<Self, TraceDecayError> {
        let project_id = project_id.ok_or_else(|| {
            TraceDecayError::project_route(
                CODE_INDEX_IDENTITY_MISMATCH,
                false,
                "branch graph publication requires an authoritative project identity",
            )
        })?;
        let project_id = ProjectId::new(project_id.to_owned()).map_err(|error| {
            TraceDecayError::project_route(
                CODE_INDEX_IDENTITY_MISMATCH,
                false,
                format!(
                    "branch graph publication has an invalid project identity '{project_id}': {error}"
                ),
            )
        })?;
        Ok(Self {
            project_id,
            project_root: project_root.to_path_buf(),
            data_root: data_root.to_path_buf(),
        })
    }

    /// Seal and publish the exact generation currently mounted for a branch worktree.
    #[hotpath::measure(label = "daemon.code_index.branch_publication.track", future = true)]
    pub async fn track_exact_worktree_branch(
        &self,
        schedulers: &CodeIndexSchedulerRegistryV1,
        project_root: &Path,
        worktree_root: &Path,
        branch: &str,
        cancellation: &CancellationToken,
    ) -> Result<BranchAddOutcome, TraceDecayError> {
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
        if !self.owns_project(&canonical_project_root)? {
            return Err(TraceDecayError::project_route(
                CODE_INDEX_IDENTITY_MISMATCH,
                false,
                format!(
                    "branch project root '{}' is not owned by the retained project graph",
                    canonical_project_root.display()
                ),
            ));
        }
        if cancellation.is_cancelled() {
            return Err(branch_publication_cancelled_error(branch));
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
        let source_branch = tracedecay_runtime_core::branch::current_branch(
            &canonical_worktree_root,
        )
        .ok_or_else(|| {
            TraceDecayError::project_route(
                GIT_SNAPSHOT_UNAVAILABLE,
                false,
                format!(
                    "branch graph publication requires an attached source branch for '{}'",
                    canonical_worktree_root.display()
                ),
            )
        })?;
        let source = self
            .capture_exact_branch_source(
                schedulers,
                &canonical_project_root,
                &canonical_worktree_root,
                &source_branch,
            )
            .await?;
        if cancellation.is_cancelled() {
            return Err(branch_publication_cancelled_error(branch));
        }
        let prepared = match tracedecay_runtime_core::branch::prepare_branch_tracking_in_layout(
            &canonical_worktree_root,
            branch,
            &self.data_root,
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
            BranchTrackingPreparation::Deferred => return Ok(BranchAddOutcome::Deferred),
        };
        if cancellation.is_cancelled() {
            let error = branch_publication_cancelled_error(branch);
            self.rollback_failed_branch_tracking(prepared.as_deref(), None, &error)
                .await?;
            return Err(error);
        }
        let expected_source = tracedecay_runtime_core::branch_meta::load_branch_meta(
            &self.data_root,
        )
        .and_then(|meta| {
            meta.branches
                .get(branch)
                .and_then(|entry| entry.graph_source.clone())
        });
        let installation = match self
            .await_exact_branch_generation_installation(
                schedulers,
                &canonical_worktree_root,
                &source,
                cancellation,
            )
            .await
        {
            Ok(installation) => installation,
            Err(error) => {
                self.rollback_failed_branch_tracking(prepared.as_deref(), None, &error)
                    .await?;
                return Err(error);
            }
        };
        let publication = tracedecay_runtime_core::branch_meta::publish_graph_source(
            &self.data_root,
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
            Ok(BranchGraphSourcePublishOutcomeV1::Published(publication)) => {
                match schedulers
                    .commit_serving_generation_installation(&canonical_worktree_root, installation)
                    .await
                {
                    ServingGenerationRollbackOutcomeV1::Cleared => Ok(BranchAddOutcome::Added),
                    ServingGenerationRollbackOutcomeV1::NoMatch => {
                        let error = TraceDecayError::project_route(
                            CODE_INDEX_ACTIVATION_UNAVAILABLE,
                            true,
                            format!(
                                "serving generation changed while publishing branch '{branch}'"
                            ),
                        );
                        self.rollback_failed_branch_tracking(
                            prepared.as_deref(),
                            Some(&publication),
                            &error,
                        )
                        .await?;
                        Err(error)
                    }
                }
            }
            Ok(BranchGraphSourcePublishOutcomeV1::AlreadyPublished(_)) => {
                match schedulers
                    .commit_serving_generation_installation(&canonical_worktree_root, installation)
                    .await
                {
                    ServingGenerationRollbackOutcomeV1::Cleared => {
                        Ok(BranchAddOutcome::AlreadyTracked)
                    }
                    ServingGenerationRollbackOutcomeV1::NoMatch => {
                        Err(TraceDecayError::project_route(
                            CODE_INDEX_ACTIVATION_UNAVAILABLE,
                            true,
                            format!(
                                "serving generation changed before exact branch replay completed for '{branch}'"
                            ),
                        ))
                    }
                }
            }
            Ok(BranchGraphSourcePublishOutcomeV1::CompareAndSwapMiss {
                observed: Some(observed),
            }) if observed.matches_draft(&source) => {
                match schedulers
                    .commit_serving_generation_installation(&canonical_worktree_root, installation)
                    .await
                {
                    ServingGenerationRollbackOutcomeV1::Cleared => {
                        Ok(BranchAddOutcome::AlreadyTracked)
                    }
                    ServingGenerationRollbackOutcomeV1::NoMatch => {
                        Err(TraceDecayError::project_route(
                            CODE_INDEX_ACTIVATION_UNAVAILABLE,
                            true,
                            format!(
                                "serving generation changed before exact branch replay completed for '{branch}'"
                            ),
                        ))
                    }
                }
            }
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
                self.rollback_failed_branch_tracking(prepared.as_deref(), None, &error)
                    .await?;
                Err(error)
            }
            Err(error) => {
                let _ = schedulers
                    .commit_serving_generation_installation(&canonical_worktree_root, installation)
                    .await;
                self.rollback_failed_branch_tracking(prepared.as_deref(), None, &error)
                    .await?;
                Err(error)
            }
        }
    }

    /// Capture the exact Git identity for a mounted branch worktree.
    #[hotpath::measure(
        label = "daemon.code_index.branch_publication.capture_source",
        future = true
    )]
    pub(super) async fn capture_exact_branch_source(
        &self,
        schedulers: &CodeIndexSchedulerRegistryV1,
        canonical_project_root: &Path,
        canonical_worktree_root: &Path,
        branch: &str,
    ) -> Result<BranchGraphSourceDraftV1, TraceDecayError> {
        if !self.owns_project(canonical_project_root)? {
            return Err(TraceDecayError::project_route(
                CODE_INDEX_IDENTITY_MISMATCH,
                false,
                format!(
                    "branch project root '{}' is not owned by the retained project graph",
                    canonical_project_root.display()
                ),
            ));
        }
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
        let snapshot = crate::git_transactions::capture_exact_snapshot(
            canonical_worktree_root,
            self.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            tracedecay_contracts::now_micros(),
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
        if snapshot.project_id != self.project_id
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
        Ok(BranchGraphSourceDraftV1 {
            project_id: self.project_id.as_str().to_owned(),
            repository_id: scope.repository_id.as_str().to_owned(),
            worktree_id: scope.worktree_id.as_str().to_owned(),
            worktree_root: canonical_worktree_root.to_string_lossy().into_owned(),
            reference: snapshot_branch,
            source_oid,
        })
    }

    async fn await_exact_branch_generation_installation(
        &self,
        schedulers: &CodeIndexSchedulerRegistryV1,
        canonical_worktree_root: &Path,
        source: &BranchGraphSourceDraftV1,
        cancellation: &CancellationToken,
    ) -> Result<ServingGenerationInstallationV1, TraceDecayError> {
        let mut serving_changes = schedulers
            .subscribe_serving_generation_changes(canonical_worktree_root)
            .await
            .ok_or_else(|| {
                TraceDecayError::project_route(
                    CODE_INDEX_SCHEDULER_UNAVAILABLE,
                    true,
                    format!(
                        "code-index scheduler is unavailable for branch worktree '{}'",
                        canonical_worktree_root.display()
                    ),
                )
            })?;
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
        let hard_deadline = Instant::now() + BRANCH_GENERATION_HARD_TIMEOUT;
        let mut idle_deadline = Instant::now() + BRANCH_GENERATION_IDLE_TIMEOUT;
        loop {
            if cancellation.is_cancelled() {
                return Err(branch_publication_cancelled_error(&source.reference));
            }
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
            let freshness = schedulers
                .dashboard_freshness(canonical_worktree_root)
                .await;
            if let Some(generation) = scope
                .serving_generation
                .as_ref()
                .filter(|generation| generation_matches_branch_source(generation, source))
                && freshness.as_ref().is_some_and(|freshness| {
                    freshness.latest_generation_id.as_deref()
                        == Some(generation.manifest().generation_id.as_str())
                        && matches!(
                            freshness.code_graph_serving.as_ref(),
                            Some(CodeGraphServingReadinessV1::Ready)
                        )
                })
                && let ServingGenerationInstallationOutcomeV1::Installed(installation) = schedulers
                    .install_exact_serving_generation(canonical_worktree_root, generation)
                    .await
            {
                return Ok(installation);
            }
            let now = Instant::now();
            if now >= hard_deadline {
                return Err(branch_generation_timeout_error(
                    canonical_worktree_root,
                    source,
                    &scope,
                    freshness.as_ref(),
                ));
            }
            if freshness
                .as_ref()
                .is_some_and(branch_generation_work_is_active)
            {
                idle_deadline = now + BRANCH_GENERATION_IDLE_TIMEOUT;
            } else if now >= idle_deadline {
                return Err(branch_generation_timeout_error(
                    canonical_worktree_root,
                    source,
                    &scope,
                    freshness.as_ref(),
                ));
            }
            tokio::select! {
                () = cancellation.cancelled() => {
                    return Err(branch_publication_cancelled_error(&source.reference));
                }
                result = serving_changes.changed() => {
                    if result.is_err() {
                        return Err(TraceDecayError::project_route(
                            CODE_INDEX_ACTIVATION_UNAVAILABLE,
                            true,
                            format!(
                                "code-index serving owner closed for branch worktree '{}'",
                                canonical_worktree_root.display()
                            ),
                        ));
                    }
                }
                () = tokio::time::sleep_until(idle_deadline.min(hard_deadline)) => {}
            }
        }
    }

    async fn rollback_failed_branch_tracking(
        &self,
        prepared: Option<&tracedecay_runtime_core::branch::PreparedBranchTracking>,
        publication: Option<&BranchGraphSourcePublicationV1>,
        cause: &TraceDecayError,
    ) -> Result<(), TraceDecayError> {
        let publication_rolled_back = match publication {
            Some(publication) => {
                match tracedecay_runtime_core::branch_meta::rollback_graph_source_publication(
                    &self.data_root,
                    publication,
                )
                .map_err(|error| {
                    TraceDecayError::project_route(
                        BRANCH_TRACKING_FAILED,
                        true,
                        format!(
                            "branch publication failed: {cause}; source rollback failed: {error}"
                        ),
                    )
                })? {
                    BranchGraphSourceRollbackOutcomeV1::Restored => true,
                    BranchGraphSourceRollbackOutcomeV1::NoMatch => false,
                }
            }
            None => true,
        };
        if !publication_rolled_back {
            return Ok(());
        }
        if let Some(prepared) = prepared {
            match tracedecay_runtime_core::branch::rollback_prepared_branch_tracking(
                &self.data_root,
                prepared,
            )
            .map_err(|error| {
                TraceDecayError::project_route(
                    BRANCH_TRACKING_FAILED,
                    true,
                    format!("branch publication failed: {cause}; branch rollback failed: {error}"),
                )
            })? {
                PreparedBranchRollbackOutcome::RolledBack
                | PreparedBranchRollbackOutcome::NoMatch => {}
            }
        }
        Ok(())
    }

    fn owns_project(&self, canonical_root: &Path) -> Result<bool, TraceDecayError> {
        let retained_root =
            self.project_root
                .canonicalize()
                .map_err(|error| TraceDecayError::File {
                    message: format!(
                        "failed to canonicalize retained branch project root: {error}"
                    ),
                    path: self.project_root.display().to_string(),
                })?;
        Ok(retained_root == canonical_root)
    }
}

pub(super) fn branch_generation_work_is_active(freshness: &CodeIndexWorktreeFreshnessV1) -> bool {
    freshness.rebuild_in_flight
        || matches!(
            freshness.code_graph_serving,
            Some(CodeGraphServingReadinessV1::Pending)
        )
}

fn branch_generation_timeout_error(
    canonical_worktree_root: &Path,
    source: &BranchGraphSourceDraftV1,
    scope: &CodeIndexServingScopeV1,
    freshness: Option<&CodeIndexWorktreeFreshnessV1>,
) -> TraceDecayError {
    let expected = serde_json::json!({
        "project": source.project_id,
        "repository": source.repository_id,
        "worktree": source.worktree_id,
        "root": canonical_worktree_root.display().to_string(),
        "ref": source.reference,
        "revision": source.source_oid,
    });
    TraceDecayError::project_route(
        CODE_INDEX_ACTIVATION_UNAVAILABLE,
        true,
        format!(
            "code-index scheduler did not publish exact branch source: expected={expected} observed={}",
            branch_generation_observation(scope, freshness),
        ),
    )
}

fn branch_generation_observation(
    scope: &CodeIndexServingScopeV1,
    freshness: Option<&CodeIndexWorktreeFreshnessV1>,
) -> serde_json::Value {
    let serving = scope.serving_generation.as_deref();
    let serving_snapshot = serving.map(CodeIndexPublishedGenerationV1::snapshot);
    let terminal_error = freshness.and_then(|freshness| {
        freshness
            .parked
            .as_ref()
            .map(|parked| parked.reason.as_str())
            .or_else(|| match freshness.code_graph_serving.as_ref() {
                Some(CodeGraphServingReadinessV1::Refused { reason }) => Some(reason.as_str()),
                Some(CodeGraphServingReadinessV1::Unavailable { reason })
                    if !freshness.rebuild_in_flight =>
                {
                    Some(reason.as_str())
                }
                _ => None,
            })
    });
    serde_json::json!({
        "mounted": {
            "repository": scope.repository_id.as_str(),
            "worktree": scope.worktree_id.as_str(),
        },
        "serving": serving.map(|generation| serde_json::json!({
            "project": generation.manifest().project_id.as_str(),
            "repository": serving_snapshot.map(|snapshot| snapshot.repository.as_str()),
            "worktree": serving_snapshot.and_then(|snapshot| snapshot.worktree.as_ref()),
            "ref": serving_snapshot.and_then(|snapshot| snapshot.reference.as_ref()),
            "revision": serving_snapshot.and_then(|snapshot| snapshot.source_revision.as_ref()),
            "generation": generation.manifest().generation_id.as_str(),
        })),
        "freshness": freshness,
        "terminal_error": terminal_error,
    })
}

fn generation_matches_branch_source(
    generation: &CodeIndexPublishedGenerationV1,
    source: &BranchGraphSourceDraftV1,
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
