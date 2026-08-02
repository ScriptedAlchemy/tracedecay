//! Concrete bridge from daemon transaction orchestration to fixed native Git.
//!
//! Preview material stays only in daemon memory. A restart therefore forces a
//! fresh preview for any unstarted apply; the durable journal handles only
//! transactions that reached native admission.

use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::Serialize;
use tracedecay_application::{
    GitIndexApplyRequestV1, GitIndexPreviewPortResultV1, GitIndexPreviewRequestV1,
    GitIndexTransactionPortError, OperationBudgetUsage, OperationReceipt, OperationTermination,
};
use tracedecay_domain::{
    GitCommitIdentityV1, GitDegradationV1, GitDiffScopeV1, GitHeadStateV1, GitIndexCommitIntentV1,
    GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1, GitIndexReceiptId,
    GitIndexReceiptOutcomeV1, GitIndexSigningPolicyV1, GitIndexTransactionId,
    GitIndexTransactionOperationV1, GitIndexTransactionReceiptV1, GitIndexUnsupportedStateV1,
    GitStatusEntryV1, ManifestDigest, ProjectId, RepositoryId, RepositoryIndexSnapshotV1,
    RepositoryIndexStateV1, RepositoryStateSnapshotV1, RepositoryWorkingTreeSnapshotV1,
    RepositoryWorkingTreeStateV1, UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_store::GitIndexTransactionRecordV1;

use crate::git_index_transactions::{
    FixedGitIndexRunner, GIT_INDEX_ADAPTER_REVISION, NativeGitIndexError, NativeIndexLock,
    ValidatedIndexPatch,
};
use crate::git_intelligence::NativeGitIntelligence;

use super::service::NativeGitIndexApplyOutcomeV1;
use super::{
    GitIndexNativeExecutor, GitIndexRecoveryError, GitIndexRecoveryExecutor,
    NativeGitIndexApplyResult,
};

/// Native preview state that has not crossed a durable mutation boundary.
#[derive(Clone, Debug)]
pub(crate) struct MaterializedGitIndexPreview {
    pub preview: GitIndexPreviewV1,
    pub execution: OperationReceipt,
    pub commit_intent: Option<GitIndexCommitIntentV1>,
    pub(crate) runner: FixedGitIndexRunner,
    pub(crate) patches: Vec<ValidatedIndexPatch>,
}

/// Repository-specific preview and snapshot authority. Implementations may use
/// only the fixed PR11 native adapter to build patch material and capture
/// state; no transport data or arbitrary Git input reaches this boundary.
pub(crate) trait GitIndexPreviewAssembler {
    fn materialize(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<MaterializedGitIndexPreview, GitIndexTransactionPortError>;

    fn capture_current(
        &self,
        preview: &MaterializedGitIndexPreview,
        lock: &NativeIndexLock,
    ) -> Result<RepositoryStateSnapshotV1, GitIndexTransactionPortError>;

    fn revalidate_patches(
        &self,
        preview: &MaterializedGitIndexPreview,
    ) -> Result<Vec<ValidatedIndexPatch>, GitIndexTransactionPortError>;

    fn finalize(
        &self,
        preview: &MaterializedGitIndexPreview,
        transaction_id: &GitIndexTransactionId,
        request: &GitIndexApplyRequestV1,
        created_commit: Option<&tracedecay_domain::GitOidV1>,
    ) -> Result<NativeGitIndexApplyResult, GitIndexTransactionPortError>;

    fn reconcile(
        &self,
        record: &GitIndexTransactionRecordV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError>;
}

/// Concrete PR11 assembler backed by the fixed query read-only authority and
/// the isolated-index preview mechanics in [`FixedGitIndexRunner`].
pub(crate) struct NativeGitIndexPreviewAssembler {
    repository_root: PathBuf,
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
}

impl NativeGitIndexPreviewAssembler {
    pub(crate) fn new(
        repository_root: impl Into<PathBuf>,
        project_id: ProjectId,
        repository_id: RepositoryId,
        worktree_id: WorktreeId,
    ) -> Self {
        let repository_root = repository_root.into();
        // Capture and daemon recapture must share one filesystem identity.
        // Fall back only when the path cannot be resolved yet; existing roots
        // (including symlink aliases) always canonicalize.
        let repository_root =
            super::canonicalize_repository_root(&repository_root).unwrap_or(repository_root);
        Self {
            repository_root,
            project_id,
            repository_id,
            worktree_id,
        }
    }

    fn read_authority(&self) -> NativeGitIntelligence {
        NativeGitIntelligence::new(
            self.repository_root.clone(),
            self.repository_id.clone(),
            self.worktree_id.clone(),
        )
    }

    fn runner(&self) -> Result<FixedGitIndexRunner, GitIndexTransactionPortError> {
        FixedGitIndexRunner::new(&self.repository_root).map_err(map_native_error)
    }

    fn capture_snapshot(
        &self,
        template: &RepositoryStateSnapshotV1,
        runner: &FixedGitIndexRunner,
        lock: &NativeIndexLock,
    ) -> Result<RepositoryStateSnapshotV1, GitIndexTransactionPortError> {
        if template.project_id != self.project_id
            || template.repository_id != self.repository_id
            || template.worktree_id.as_ref() != Some(&self.worktree_id)
        {
            return Err(GitIndexTransactionPortError::StalePreview);
        }
        // Failing to read status at all says nothing about whether the caller's
        // snapshot still holds; it says we could not look. Calling that staleness
        // sent callers to recapture and retry a read that fails identically.
        let status = self
            .read_authority()
            .status()
            .map_err(|_| GitIndexTransactionPortError::NativeFailure)?;
        let index_bytes = runner.index_bytes().map_err(map_native_error)?;
        let index_checksum = canonical_sha256(&index_bytes)
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let index_tree = runner
            .index_tree_under_lock(lock)
            .map_err(map_native_error)?;

        let tracked = status
            .entries
            .iter()
            .filter(|entry| matches!(entry, GitStatusEntryV1::Tracked(_)))
            .collect::<Vec<_>>();
        let untracked = status
            .entries
            .iter()
            .filter_map(|entry| match entry {
                GitStatusEntryV1::Untracked { path } => Some(path),
                _ => None,
            })
            .collect::<Vec<_>>();
        let tracked_digest = runner.tracked_worktree_digest().map_err(map_native_error)?;
        let untracked_name_digest = (!untracked.is_empty())
            .then(|| canonical_sha256(&untracked))
            .transpose()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let ignored = status
            .entries
            .iter()
            .filter_map(|entry| match entry {
                GitStatusEntryV1::Ignored { path } => Some(path),
                _ => None,
            })
            .collect::<Vec<_>>();
        let ignored_collision_digest = (!ignored.is_empty())
            .then(|| canonical_sha256(&ignored))
            .transpose()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;

        let index_state = if status.coverage.records(GitDegradationV1::SplitIndex) {
            RepositoryIndexStateV1::Split
        } else if status.coverage.records(GitDegradationV1::SparseCheckout) {
            RepositoryIndexStateV1::Sparse
        } else if status.conflicted_count() > 0 {
            RepositoryIndexStateV1::Unmerged
        } else if runner.has_intent_to_add().map_err(map_native_error)? {
            RepositoryIndexStateV1::IntentToAdd
        } else if status.staged_count() > 0 {
            RepositoryIndexStateV1::Staged
        } else {
            RepositoryIndexStateV1::Clean
        };
        let unmerged_stage_digest = (index_state == RepositoryIndexStateV1::Unmerged)
            .then(|| canonical_sha256(&tracked))
            .transpose()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;

        let working_tree_state = match (
            status.conflicted_count(),
            status.unstaged_count(),
            status.untracked_count(),
        ) {
            (conflicts, _, _) if conflicts > 0 => RepositoryWorkingTreeStateV1::Conflicted,
            (0, 0, 0) => RepositoryWorkingTreeStateV1::Clean,
            (0, 0, _) => RepositoryWorkingTreeStateV1::UntrackedOnly,
            (0, _, 0) => RepositoryWorkingTreeStateV1::TrackedDirty,
            (0, _, _) => RepositoryWorkingTreeStateV1::Mixed,
            _ => RepositoryWorkingTreeStateV1::Unreadable,
        };

        let configuration_digest = runner.configuration_digest().map_err(map_native_error)?;
        let head = runner.head_state().map_err(map_native_error)?;
        RepositoryStateSnapshotV1::new(
            self.project_id.clone(),
            self.repository_id.clone(),
            Some(self.worktree_id.clone()),
            template.observation_epoch,
            index_tree.format(),
            head,
            RepositoryIndexSnapshotV1 {
                checksum: index_checksum,
                tree_id: Some(index_tree),
                state: index_state,
                unmerged_stage_digest,
            },
            RepositoryWorkingTreeSnapshotV1 {
                state: working_tree_state,
                tracked_digest,
                untracked_name_digest,
                ignored_collision_digest,
            },
            status.operation,
            Some(configuration_digest.clone()),
            Some(runner.sparse_digest().map_err(map_native_error)?),
            Some(runner.submodule_digest().map_err(map_native_error)?),
            Some(configuration_digest),
            // Observation metadata belongs to the caller's read snapshot. All
            // repository facts above are independently recaptured; retaining
            // its timestamp permits exact byte-for-byte CAS equality.
            template.captured_at,
            status.coverage,
        )
        .and_then(|snapshot| {
            snapshot.with_native_identity(
                runner
                    .git_version()
                    .map_err(|_| tracedecay_domain::DomainError::NonCanonical {
                        field: "repository git version",
                    })?,
                GIT_INDEX_ADAPTER_REVISION.to_owned(),
                runner
                    .refs_digest()
                    .map_err(|_| tracedecay_domain::DomainError::NonCanonical {
                        field: "repository refs digest",
                    })?,
            )
        })
        // Constructing our own snapshot from state we just read, or failing to
        // read the git version or refs digest, is this adapter failing rather
        // than the repository moving under the caller.
        .map_err(|_| GitIndexTransactionPortError::NativeFailure)
    }

    fn materialize_patches(
        &self,
        request: &GitIndexPreviewRequestV1,
        snapshot_digest: &ManifestDigest,
    ) -> Result<Vec<ValidatedIndexPatch>, GitIndexTransactionPortError> {
        self.materialize_selected_patches(
            request.binding.operation,
            request.preview_id.as_str(),
            &request.selected_hunks,
            snapshot_digest,
        )
    }

    fn materialize_selected_patches(
        &self,
        operation: GitIndexTransactionOperationV1,
        preview_id: &str,
        selected_hunks: &[tracedecay_domain::HunkRefV1],
        snapshot_digest: &ManifestDigest,
    ) -> Result<Vec<ValidatedIndexPatch>, GitIndexTransactionPortError> {
        let scope = match operation {
            GitIndexTransactionOperationV1::StageHunks => GitDiffScopeV1::WorkingTree,
            GitIndexTransactionOperationV1::UnstageHunks => GitDiffScopeV1::Staged,
            GitIndexTransactionOperationV1::CommitIndex => return Ok(Vec::new()),
        };
        let current_refs = self
            .read_authority()
            .hunk_refs(&scope, preview_id, snapshot_digest)
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let mut patches = Vec::with_capacity(selected_hunks.len());
        for requested in selected_hunks {
            let requested_digest = requested
                .compute_digest()
                .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
            let current = current_refs
                .iter()
                .find(|reference| {
                    reference
                        .compute_digest()
                        .is_ok_and(|digest| digest == requested_digest)
                })
                .ok_or(GitIndexTransactionPortError::StalePreview)?;
            if current != requested {
                return Err(GitIndexTransactionPortError::StalePreview);
            }
            let bytes = extract_patch(&self.repository_root, &scope, requested)?;
            patches.push(
                ValidatedIndexPatch::new(requested.clone(), bytes).map_err(map_native_error)?,
            );
        }
        let mut keyed = patches
            .into_iter()
            .map(|patch| {
                patch
                    .hunk()
                    .compute_digest()
                    .map(|digest| (digest, patch))
                    .map_err(|_| GitIndexTransactionPortError::StalePreview)
            })
            .collect::<Result<Vec<_>, _>>()?;
        keyed.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(keyed.into_iter().map(|(_, patch)| patch).collect())
    }
}

/// Project-owned assembler used by the daemon singleton before PR12 adds a
/// transport binding. Repository and worktree identities come from the typed
/// request scope or durable transaction record; the daemon contributes only
/// the authoritative project identity and currently opened worktree root.
pub(crate) struct DaemonProjectGitIndexPreviewAssembler {
    repository_root: PathBuf,
    project_id: ProjectId,
}

impl DaemonProjectGitIndexPreviewAssembler {
    pub(crate) fn new(repository_root: impl Into<PathBuf>, project_id: ProjectId) -> Self {
        let repository_root = repository_root.into();
        let repository_root =
            super::canonicalize_repository_root(&repository_root).unwrap_or(repository_root);
        Self {
            repository_root,
            project_id,
        }
    }

    fn for_preview(
        &self,
        preview: &GitIndexPreviewV1,
    ) -> Result<NativeGitIndexPreviewAssembler, GitIndexTransactionPortError> {
        if preview.repository_snapshot.project_id != self.project_id {
            return Err(GitIndexTransactionPortError::StalePreview);
        }
        let worktree_id = preview
            .repository_snapshot
            .worktree_id
            .clone()
            .ok_or(GitIndexTransactionPortError::StalePreview)?;
        Ok(NativeGitIndexPreviewAssembler::new(
            self.repository_root.clone(),
            self.project_id.clone(),
            preview.repository_snapshot.repository_id.clone(),
            worktree_id,
        ))
    }
}

impl GitIndexPreviewAssembler for DaemonProjectGitIndexPreviewAssembler {
    fn materialize(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<MaterializedGitIndexPreview, GitIndexTransactionPortError> {
        let scope = request.context.scope();
        if scope.project_id != self.project_id {
            // The daemon has no tracing subscriber; its diagnostic channel is
            // this event line, so anything emitted through tracing here is
            // unreadable in the process that runs it.
            eprintln!(
                "[tracedecay] event=git_index_preview_project_mismatch requested={} mounted={}",
                scope.project_id, self.project_id
            );
            return Err(GitIndexTransactionPortError::StalePreview);
        }
        NativeGitIndexPreviewAssembler::new(
            self.repository_root.clone(),
            self.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
        )
        .materialize(request)
    }

    fn capture_current(
        &self,
        preview: &MaterializedGitIndexPreview,
        lock: &NativeIndexLock,
    ) -> Result<RepositoryStateSnapshotV1, GitIndexTransactionPortError> {
        self.for_preview(&preview.preview)?
            .capture_current(preview, lock)
    }

    fn revalidate_patches(
        &self,
        preview: &MaterializedGitIndexPreview,
    ) -> Result<Vec<ValidatedIndexPatch>, GitIndexTransactionPortError> {
        self.for_preview(&preview.preview)?
            .revalidate_patches(preview)
    }

    fn finalize(
        &self,
        preview: &MaterializedGitIndexPreview,
        transaction_id: &GitIndexTransactionId,
        request: &GitIndexApplyRequestV1,
        created_commit: Option<&tracedecay_domain::GitOidV1>,
    ) -> Result<NativeGitIndexApplyResult, GitIndexTransactionPortError> {
        self.for_preview(&preview.preview)?.finalize(
            preview,
            transaction_id,
            request,
            created_commit,
        )
    }

    fn reconcile(
        &self,
        record: &GitIndexTransactionRecordV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError> {
        self.for_preview(&record.preview)
            .map_err(|_| GitIndexRecoveryError::Indeterminate)?
            .reconcile(record)
    }
}

impl GitIndexPreviewAssembler for NativeGitIndexPreviewAssembler {
    fn materialize(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<MaterializedGitIndexPreview, GitIndexTransactionPortError> {
        request
            .validate()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let runner = self.runner()?;
        if unsupported_native_preflight(&runner)?.is_some() {
            // Bare repositories, unsupported object formats, and an index
            // owned by another process cannot be recaptured under our lock.
            // Returning a preview here would falsely bless the caller's
            // snapshot as current.
            return Err(GitIndexTransactionPortError::Unsupported);
        }
        let lock = match runner.acquire_index_lock() {
            Ok(lock) => lock,
            Err(NativeGitIndexError::IndexLocked) => {
                return Err(GitIndexTransactionPortError::Unsupported);
            }
            Err(error) => return Err(map_native_error(error)),
        };
        let current = self.capture_snapshot(&request.repository_snapshot, &runner, &lock)?;
        if current != request.repository_snapshot {
            if let (
                Ok(serde_json::Value::Object(recaptured)),
                Ok(serde_json::Value::Object(requested)),
            ) = (
                serde_json::to_value(&current),
                serde_json::to_value(&request.repository_snapshot),
            ) {
                for (field, recaptured_value) in &recaptured {
                    if requested.get(field) != Some(recaptured_value) {
                        eprintln!(
                            "[tracedecay] event=git_index_preview_snapshot_field_changed field={field} recaptured={recaptured_value} requested={:?}",
                            requested.get(field)
                        );
                    }
                }
            }
            return Err(GitIndexTransactionPortError::StalePreview);
        }
        if let Some(reason) = unsupported_commit_preflight(request, &runner)? {
            drop(lock);
            return unsupported_materialized(request, runner, reason);
        }
        // The snapshot already matched the caller's byte for byte above, so a
        // digest we cannot compute over it is our own canonicalization
        // failing, not the repository moving underneath the request.
        let snapshot_digest = GitIndexPreviewV1::repository_snapshot_digest(&current)
            .map_err(|_| GitIndexTransactionPortError::NativeFailure)?;
        let disposition = unsupported_hunk_selection(
            &request.selected_hunks,
            &runner,
            &self.read_authority(),
            request.binding.operation,
        )
        .or_else(|| unsupported_state(&current, &runner))
        .map_or(
            GitIndexPreviewDispositionV1::Applicable,
            GitIndexPreviewDispositionV1::Unsupported,
        );
        let (selected_hunks, patches, candidate_index_tree) = if disposition.is_applicable() {
            let patches = self.materialize_patches(request, &snapshot_digest)?;
            let selected_hunks = patches
                .iter()
                .map(|patch| patch.hunk().clone())
                .collect::<Vec<_>>();
            let candidate_index_tree = match request.binding.operation {
                GitIndexTransactionOperationV1::StageHunks => Some(
                    runner
                        .preview_candidate_tree_under_lock(&lock, &patches, false)
                        .map_err(map_native_error)?,
                ),
                GitIndexTransactionOperationV1::UnstageHunks => Some(
                    runner
                        .preview_candidate_tree_under_lock(&lock, &patches, true)
                        .map_err(map_native_error)?,
                ),
                GitIndexTransactionOperationV1::CommitIndex => current.index.tree_id.clone(),
            };
            (selected_hunks, patches, candidate_index_tree)
        } else {
            (Vec::new(), Vec::new(), None)
        };
        let expires_at = UtcMicros(request.observed_at.0.saturating_add(30_000_000));
        let preview = GitIndexPreviewV1::new_with_commit_intent(
            request.preview_id.clone(),
            request.binding.operation,
            current,
            snapshot_digest,
            selected_hunks,
            candidate_index_tree,
            request.commit_intent.as_ref(),
            disposition,
            request.observed_at,
            expires_at,
        )
        // Every input here is either the caller's own request or state we just
        // recaptured and matched, so a rejected construction — a commit intent
        // the preview will not carry, most often — is a rejection of the
        // request rather than evidence that it went stale.
        .map_err(|_| GitIndexTransactionPortError::NativeFailure)?;
        Ok(MaterializedGitIndexPreview {
            preview,
            execution: completed_execution(request),
            commit_intent: request.commit_intent.clone(),
            runner,
            patches,
        })
    }

    fn capture_current(
        &self,
        preview: &MaterializedGitIndexPreview,
        lock: &NativeIndexLock,
    ) -> Result<RepositoryStateSnapshotV1, GitIndexTransactionPortError> {
        self.capture_snapshot(&preview.preview.repository_snapshot, &preview.runner, lock)
    }

    fn revalidate_patches(
        &self,
        preview: &MaterializedGitIndexPreview,
    ) -> Result<Vec<ValidatedIndexPatch>, GitIndexTransactionPortError> {
        self.materialize_selected_patches(
            preview.preview.operation,
            preview.preview.preview_id.as_str(),
            &preview.preview.selected_hunks,
            &preview.preview.repository_snapshot_digest,
        )
    }

    fn finalize(
        &self,
        preview: &MaterializedGitIndexPreview,
        transaction_id: &GitIndexTransactionId,
        request: &GitIndexApplyRequestV1,
        created_commit: Option<&tracedecay_domain::GitOidV1>,
    ) -> Result<NativeGitIndexApplyResult, GitIndexTransactionPortError> {
        let lock = preview
            .runner
            .acquire_index_lock()
            .map_err(map_native_error)?;
        let current =
            self.capture_snapshot(&preview.preview.repository_snapshot, &preview.runner, &lock)?;
        if !live_result_matches_preview(
            &self.repository_root,
            &preview.preview,
            &current,
            created_commit,
        ) {
            return Err(GitIndexTransactionPortError::NeedsInspection);
        }
        let final_snapshot_digest = GitIndexPreviewV1::repository_snapshot_digest(&current)
            .map_err(|_| GitIndexTransactionPortError::NeedsInspection)?;
        let receipt = GitIndexTransactionReceiptV1::new(
            receipt_id(transaction_id)?,
            transaction_id.clone(),
            &preview.preview,
            final_snapshot_digest,
            current.index.tree_id.clone(),
            current.head.commit().cloned(),
            created_commit.cloned(),
            GitIndexReceiptOutcomeV1::Committed,
            request.observed_at,
        )
        .map_err(|_| GitIndexTransactionPortError::NeedsInspection)?;
        Ok(NativeGitIndexApplyResult {
            receipt,
            execution: completed_apply_execution(request),
        })
    }

    fn reconcile(
        &self,
        record: &GitIndexTransactionRecordV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError> {
        let runner = FixedGitIndexRunner::new(&self.repository_root)
            .map_err(|_| GitIndexRecoveryError::Indeterminate)?;
        let lock = runner
            .acquire_index_lock()
            .map_err(|_| GitIndexRecoveryError::Indeterminate)?;
        let current = self
            .capture_snapshot(&record.preview.repository_snapshot, &runner, &lock)
            .map_err(|_| GitIndexRecoveryError::Indeterminate)?;
        let old = &record.preview.repository_snapshot;
        let phase = record.journal.phase;
        let (outcome, created_commit) = if &current == old
            && matches!(
                phase,
                tracedecay_domain::GitIndexJournalPhaseV1::Prepared
                    | tracedecay_domain::GitIndexJournalPhaseV1::NativeApplyStarted
                    | tracedecay_domain::GitIndexJournalPhaseV1::NeedsInspection
            ) {
            (GitIndexReceiptOutcomeV1::AbortedNoChange, None)
        } else if record.preview.operation != GitIndexTransactionOperationV1::CommitIndex
            && phase.permits_recovered_outcome(
                record.preview.operation,
                GitIndexReceiptOutcomeV1::Committed,
            )
            && hunk_commit_matches_preview(&current, old, &record.preview)
        {
            (GitIndexReceiptOutcomeV1::Committed, None)
        } else if record.preview.operation == GitIndexTransactionOperationV1::CommitIndex
            && phase.permits_recovered_outcome(
                record.preview.operation,
                GitIndexReceiptOutcomeV1::Committed,
            )
            && commit_matches_preview(&self.repository_root, old, &record.preview, &current)
        {
            (
                GitIndexReceiptOutcomeV1::Committed,
                current.head.commit().cloned(),
            )
        } else {
            (GitIndexReceiptOutcomeV1::NeedsInspection, None)
        };
        let final_snapshot_digest = GitIndexPreviewV1::repository_snapshot_digest(&current)?;
        GitIndexTransactionReceiptV1::new(
            receipt_id(&record.journal.transaction_id)
                .map_err(|_| GitIndexRecoveryError::Indeterminate)?,
            record.journal.transaction_id.clone(),
            &record.preview,
            final_snapshot_digest,
            current.index.tree_id,
            current.head.commit().cloned(),
            created_commit,
            outcome,
            record.journal.updated_at,
        )
        .map_err(GitIndexRecoveryError::Domain)
    }
}

fn completed_execution(request: &GitIndexPreviewRequestV1) -> OperationReceipt {
    OperationReceipt {
        started_at: request.observed_at,
        ended_at: request.observed_at,
        effective_deadline: request.context.deadline().clone(),
        cancellation: None,
        budget: OperationBudgetUsage::default(),
        termination: OperationTermination::Completed,
    }
}

fn completed_apply_execution(request: &GitIndexApplyRequestV1) -> OperationReceipt {
    OperationReceipt {
        started_at: request.observed_at,
        ended_at: request.observed_at,
        effective_deadline: request.context.deadline().clone(),
        cancellation: None,
        budget: OperationBudgetUsage::default(),
        termination: OperationTermination::Completed,
    }
}

fn receipt_id(
    transaction_id: &GitIndexTransactionId,
) -> Result<GitIndexReceiptId, GitIndexTransactionPortError> {
    GitIndexReceiptId::new(format!("git-index-receipt.v1.{}", transaction_id.as_str()))
        .map_err(|_| GitIndexTransactionPortError::NeedsInspection)
}

fn unsupported_state(
    snapshot: &RepositoryStateSnapshotV1,
    _runner: &FixedGitIndexRunner,
) -> Option<GitIndexUnsupportedStateV1> {
    if snapshot
        .coverage
        .records(GitDegradationV1::UnsupportedObjectFormat)
    {
        return Some(GitIndexUnsupportedStateV1::UnsupportedObjectFormat);
    }
    if snapshot.coverage.records(GitDegradationV1::SubmoduleState) {
        return Some(GitIndexUnsupportedStateV1::Submodule);
    }
    match snapshot.head {
        GitHeadStateV1::Detached { .. } => {
            return Some(GitIndexUnsupportedStateV1::DetachedHead);
        }
        GitHeadStateV1::Unborn { .. } => {
            return Some(GitIndexUnsupportedStateV1::UnbornBranch);
        }
        GitHeadStateV1::Attached { .. } => {}
    }
    match snapshot.index.state {
        RepositoryIndexStateV1::Unmerged => Some(GitIndexUnsupportedStateV1::UnmergedIndex),
        RepositoryIndexStateV1::IntentToAdd => Some(GitIndexUnsupportedStateV1::IntentToAdd),
        RepositoryIndexStateV1::Split => Some(GitIndexUnsupportedStateV1::SplitIndex),
        RepositoryIndexStateV1::Sparse => Some(GitIndexUnsupportedStateV1::SparseIndex),
        RepositoryIndexStateV1::Unreadable => Some(GitIndexUnsupportedStateV1::UnreadableIndex),
        RepositoryIndexStateV1::Clean | RepositoryIndexStateV1::Staged => {
            match snapshot.working_tree.state {
                RepositoryWorkingTreeStateV1::Conflicted => {
                    Some(GitIndexUnsupportedStateV1::ConflictedWorkingTree)
                }
                RepositoryWorkingTreeStateV1::Unreadable => {
                    Some(GitIndexUnsupportedStateV1::UnreadableWorkingTree)
                }
                RepositoryWorkingTreeStateV1::Clean
                | RepositoryWorkingTreeStateV1::TrackedDirty
                | RepositoryWorkingTreeStateV1::UntrackedOnly
                | RepositoryWorkingTreeStateV1::Mixed => {
                    if snapshot.operation_state != tracedecay_domain::GitOperationStateV1::None {
                        Some(GitIndexUnsupportedStateV1::InProgressOperation)
                    } else if snapshot.coverage.leaves_state_unread() {
                        Some(GitIndexUnsupportedStateV1::UnreadableWorkingTree)
                    } else {
                        None
                    }
                }
            }
        }
    }
}

fn unsupported_native_preflight(
    runner: &FixedGitIndexRunner,
) -> Result<Option<GitIndexUnsupportedStateV1>, GitIndexTransactionPortError> {
    if runner.is_bare_repository().map_err(map_native_error)? {
        return Ok(Some(GitIndexUnsupportedStateV1::BareRepository));
    }
    match runner.ensure_index_unlocked() {
        Ok(()) => {}
        Err(NativeGitIndexError::IndexLocked) => {
            return Ok(Some(GitIndexUnsupportedStateV1::IndexLockPresent));
        }
        Err(error) => return Err(map_native_error(error)),
    }
    let object_format = runner.object_format().map_err(map_native_error)?;
    if !supported_object_format(&object_format) {
        return Ok(Some(GitIndexUnsupportedStateV1::UnsupportedObjectFormat));
    }
    Ok(None)
}

fn unsupported_commit_preflight(
    request: &GitIndexPreviewRequestV1,
    runner: &FixedGitIndexRunner,
) -> Result<Option<GitIndexUnsupportedStateV1>, GitIndexTransactionPortError> {
    if request.binding.operation != GitIndexTransactionOperationV1::CommitIndex {
        return Ok(None);
    }
    if runner
        .has_applicable_commit_hooks()
        .map_err(map_native_error)?
    {
        return Ok(Some(GitIndexUnsupportedStateV1::ApplicableCommitHooks));
    }
    if let Some(GitIndexSigningPolicyV1::SignatureRequired { key_reference }) = request
        .commit_intent
        .as_ref()
        .map(|intent| &intent.signing_policy)
        && !runner
            .signing_key_available(key_reference)
            .map_err(map_native_error)?
    {
        return Ok(Some(GitIndexUnsupportedStateV1::SigningKeyUnavailable));
    }
    Ok(None)
}

fn supported_object_format(format: &str) -> bool {
    matches!(format, "sha1" | "sha256")
}

fn unsupported_materialized(
    request: &GitIndexPreviewRequestV1,
    runner: FixedGitIndexRunner,
    reason: GitIndexUnsupportedStateV1,
) -> Result<MaterializedGitIndexPreview, GitIndexTransactionPortError> {
    let snapshot_digest =
        GitIndexPreviewV1::repository_snapshot_digest(&request.repository_snapshot)
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
    let preview = GitIndexPreviewV1::new_with_commit_intent(
        request.preview_id.clone(),
        request.binding.operation,
        request.repository_snapshot.clone(),
        snapshot_digest,
        Vec::new(),
        None,
        request.commit_intent.as_ref(),
        GitIndexPreviewDispositionV1::Unsupported(reason),
        request.observed_at,
        UtcMicros(request.observed_at.0.saturating_add(30_000_000)),
    )
    .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
    Ok(MaterializedGitIndexPreview {
        preview,
        execution: completed_execution(request),
        commit_intent: request.commit_intent.clone(),
        runner,
        patches: Vec::new(),
    })
}

fn unsupported_hunk_selection(
    selected_hunks: &[tracedecay_domain::HunkRefV1],
    runner: &FixedGitIndexRunner,
    intelligence: &NativeGitIntelligence,
    operation: GitIndexTransactionOperationV1,
) -> Option<GitIndexUnsupportedStateV1> {
    for hunk in selected_hunks {
        if hunk.original_path.is_some() {
            return Some(GitIndexUnsupportedStateV1::RenameOrCopy);
        }
        let line_count = normalize_hunk_header(&hunk.hunk_header)
            .and_then(|header| {
                let mut fields = header.split_whitespace();
                fields.next()?;
                let old = parse_hunk_range(fields.next()?.strip_prefix('-')?)?;
                let new = parse_hunk_range(fields.next()?.strip_prefix('+')?)?;
                Some(old.1.max(new.1))
            })
            .unwrap_or_default();
        if hunk.selected_line_bitmap != tracedecay_domain::full_hunk_selection_bitmap(line_count) {
            return Some(GitIndexUnsupportedStateV1::PartialHunkSelection);
        }
        let modes = [
            hunk.expected_index_entry.mode.as_ref(),
            hunk.expected_worktree_mode.as_ref(),
        ];
        if modes.iter().flatten().any(|mode| mode.is_submodule()) {
            return Some(GitIndexUnsupportedStateV1::Submodule);
        }
        if modes.iter().flatten().any(|mode| mode.is_symlink()) {
            return Some(GitIndexUnsupportedStateV1::Symlink);
        }
        if let Some(reason) = unsupported_path_state(runner, intelligence, operation, &hunk.path) {
            return Some(reason);
        }
    }
    None
}

fn unsupported_path_state(
    runner: &FixedGitIndexRunner,
    intelligence: &NativeGitIntelligence,
    operation: GitIndexTransactionOperationV1,
    path: &str,
) -> Option<GitIndexUnsupportedStateV1> {
    let unreadable = match operation {
        GitIndexTransactionOperationV1::StageHunks => {
            GitIndexUnsupportedStateV1::UnreadableWorkingTree
        }
        GitIndexTransactionOperationV1::UnstageHunks => GitIndexUnsupportedStateV1::UnreadableIndex,
        GitIndexTransactionOperationV1::CommitIndex => {
            GitIndexUnsupportedStateV1::UnreadableWorkingTree
        }
    };
    let scope = match operation {
        GitIndexTransactionOperationV1::StageHunks => GitDiffScopeV1::WorkingTree,
        GitIndexTransactionOperationV1::UnstageHunks => GitDiffScopeV1::Staged,
        GitIndexTransactionOperationV1::CommitIndex => {
            return Some(GitIndexUnsupportedStateV1::PartialHunkSelection);
        }
    };
    let Ok(diff) = intelligence.diff(&scope) else {
        return Some(unreadable);
    };
    let Some(file) = diff.files.iter().find(|file| file.path == path) else {
        return Some(unreadable);
    };
    if file.original_path.is_some() {
        return Some(GitIndexUnsupportedStateV1::RenameOrCopy);
    }
    if file.binary {
        return Some(GitIndexUnsupportedStateV1::BinaryHunk);
    }
    if file.submodule {
        return Some(GitIndexUnsupportedStateV1::Submodule);
    }
    if [file.old_mode.as_ref(), file.new_mode.as_ref()]
        .into_iter()
        .flatten()
        .any(tracedecay_domain::GitFileModeV1::is_symlink)
    {
        return Some(GitIndexUnsupportedStateV1::Symlink);
    }
    if file.old_mode != file.new_mode && file.hunks.is_empty() {
        return Some(GitIndexUnsupportedStateV1::FileModeOnly);
    }
    if !diff.coverage.is_complete() {
        return Some(unreadable);
    }
    let mut command = read_git_command(runner.repository_root());
    let Ok(output) = command
        .args([
            "check-attr",
            "-z",
            "filter",
            "text",
            "eol",
            "working-tree-encoding",
            "--",
            path,
        ])
        .output()
    else {
        return Some(unreadable);
    };
    if !output.status.success() {
        return Some(unreadable);
    }
    let fields = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    let mut records = fields.chunks_exact(3);
    let has_filter = records.any(|record| {
        let value = record[2];
        value != b"unspecified" && value != b"unset" && value != b"false"
    });
    if !records.remainder().is_empty() {
        return Some(unreadable);
    }
    if has_filter {
        return Some(GitIndexUnsupportedStateV1::FiltersOrEndOfLine);
    }
    None
}

#[derive(Serialize)]
struct PatchDigestMaterial<'a> {
    header: &'a str,
    body: &'a [String],
}

fn extract_patch(
    repository_root: &Path,
    scope: &GitDiffScopeV1,
    hunk: &tracedecay_domain::HunkRefV1,
) -> Result<Vec<u8>, GitIndexTransactionPortError> {
    let mut command = read_git_command(repository_root);
    command
        .arg("diff")
        .arg("--patch")
        .arg("-M")
        .arg("--no-color")
        .arg("--no-ext-diff");
    if matches!(scope, GitDiffScopeV1::Staged) {
        command.arg("--cached");
    }
    command.arg("--").arg(&hunk.path);
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
    if !output.status.success() {
        return Err(GitIndexTransactionPortError::StalePreview);
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
    let lines = text.lines().collect::<Vec<_>>();
    let mut old_marker = None;
    let mut new_marker = None;
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("--- ") {
            old_marker = Some(*line);
            new_marker = lines
                .get(index.saturating_add(1))
                .copied()
                .filter(|next| next.starts_with("+++ "));
            continue;
        }
        if !line.starts_with("@@ ") {
            continue;
        }
        let Some(normalized) = normalize_hunk_header(line) else {
            continue;
        };
        if normalized != hunk.hunk_header {
            continue;
        }
        let body = lines[index.saturating_add(1)..]
            .iter()
            .take_while(|candidate| {
                !candidate.starts_with("@@ ") && !candidate.starts_with("diff --git ")
            })
            .map(|line| (*line).to_owned())
            .collect::<Vec<_>>();
        let patch_digest = canonical_sha256(&PatchDigestMaterial {
            header: &normalized,
            body: &body,
        })
        .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let context = body
            .iter()
            .filter(|line| line.starts_with(' '))
            .map(String::as_str)
            .collect::<Vec<_>>();
        let context_digest =
            canonical_sha256(&context).map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        if patch_digest != hunk.patch_digest || context_digest != hunk.context_digest {
            continue;
        }
        let old_marker = old_marker.ok_or(GitIndexTransactionPortError::StalePreview)?;
        let new_marker = new_marker.ok_or(GitIndexTransactionPortError::StalePreview)?;
        let mut patch = format!("{old_marker}\n{new_marker}\n{normalized}\n").into_bytes();
        for line in body {
            patch.extend_from_slice(line.as_bytes());
            patch.push(b'\n');
        }
        return Ok(patch);
    }
    Err(GitIndexTransactionPortError::StalePreview)
}

fn normalize_hunk_header(header: &str) -> Option<String> {
    let mut fields = header.split_whitespace();
    (fields.next()? == "@@").then_some(())?;
    let old = parse_hunk_range(fields.next()?.strip_prefix('-')?)?;
    let new = parse_hunk_range(fields.next()?.strip_prefix('+')?)?;
    (fields.next()? == "@@").then_some(())?;
    Some(format!("@@ -{},{} +{},{} @@", old.0, old.1, new.0, new.1))
}

fn parse_hunk_range(value: &str) -> Option<(u32, u32)> {
    match value.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((value.parse().ok()?, 1)),
    }
}

fn read_git_command(repository_root: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(repository_root);
    for (key, _) in env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn commit_matches_preview(
    repository_root: &Path,
    old: &RepositoryStateSnapshotV1,
    preview: &GitIndexPreviewV1,
    current: &RepositoryStateSnapshotV1,
) -> bool {
    let (
        GitHeadStateV1::Attached {
            branch: old_branch,
            commit: old_head,
        },
        GitHeadStateV1::Attached {
            branch: current_branch,
            commit: head,
        },
        Some(expected_tree),
    ) = (
        &old.head,
        &current.head,
        preview.candidate_index_tree.as_ref(),
    )
    else {
        return false;
    };
    if old_branch != current_branch
        || current.index.tree_id.as_ref() != Some(expected_tree)
        || current.working_tree != old.working_tree
        || current.submodule_digest != old.submodule_digest
        || !same_stable_native_evidence(current, old)
    {
        return false;
    }
    let tree_expression = format!("{}^{{tree}}", head.as_str());
    let parent_expression = format!("{}^", head.as_str());
    let tree = read_git_value(repository_root, &tree_expression);
    let parent = read_git_value(repository_root, &parent_expression);
    tree.as_deref() == Some(expected_tree.as_str())
        && parent.as_deref() == Some(old_head.as_str())
        && commit_intent_matches_preview(repository_root, head, preview)
}

fn hunk_commit_matches_preview(
    current: &RepositoryStateSnapshotV1,
    old: &RepositoryStateSnapshotV1,
    preview: &GitIndexPreviewV1,
) -> bool {
    current.index.tree_id == preview.candidate_index_tree
        && current.head == old.head
        && current.refs_digest == old.refs_digest
        && same_stable_native_evidence(current, old)
}

/// Verify the complete operation-specific terminal state before emitting a
/// success receipt. Native publication proves which process crossed the
/// boundary; this observation additionally refuses success if HEAD/ref or
/// stable repository authority drifted before the terminal receipt.
fn live_result_matches_preview(
    repository_root: &Path,
    preview: &GitIndexPreviewV1,
    current: &RepositoryStateSnapshotV1,
    created_commit: Option<&tracedecay_domain::GitOidV1>,
) -> bool {
    let old = &preview.repository_snapshot;
    match preview.operation {
        GitIndexTransactionOperationV1::StageHunks
        | GitIndexTransactionOperationV1::UnstageHunks => {
            created_commit.is_none() && hunk_commit_matches_preview(current, old, preview)
        }
        GitIndexTransactionOperationV1::CommitIndex => {
            let (
                GitHeadStateV1::Attached {
                    branch: old_branch,
                    commit: old_head,
                },
                GitHeadStateV1::Attached {
                    branch: current_branch,
                    commit: current_head,
                },
                Some(created_commit),
                Some(expected_tree),
            ) = (
                &old.head,
                &current.head,
                created_commit,
                preview.candidate_index_tree.as_ref(),
            )
            else {
                return false;
            };
            if old_branch != current_branch
                || current_head != created_commit
                || current.index.tree_id.as_ref() != Some(expected_tree)
                || current.working_tree != old.working_tree
                || current.submodule_digest != old.submodule_digest
                || !same_stable_native_evidence(current, old)
            {
                return false;
            }
            let tree_expression = format!("{}^{{tree}}", created_commit.as_str());
            let parent_expression = format!("{}^", created_commit.as_str());
            read_git_value(repository_root, &tree_expression).as_deref()
                == Some(expected_tree.as_str())
                && read_git_value(repository_root, &parent_expression).as_deref()
                    == Some(old_head.as_str())
        }
    }
}

/// Compare native facts that must remain unchanged across an index-only
/// publication. Working-tree status is intentionally excluded: its digest is
/// calculated relative to the index, so a correctly published index changes
/// that observation even when no worktree byte changed. The operation-specific
/// proof owns its index/ref checks, and commit recovery additionally compares
/// its unchanged working-tree snapshot.
fn same_stable_native_evidence(
    current: &RepositoryStateSnapshotV1,
    old: &RepositoryStateSnapshotV1,
) -> bool {
    current.project_id == old.project_id
        && current.repository_id == old.repository_id
        && current.worktree_id == old.worktree_id
        && current.observation_epoch == old.observation_epoch
        && current.object_format == old.object_format
        && current.git_version == old.git_version
        && current.adapter_revision == old.adapter_revision
        && current.operation_state == old.operation_state
        && current.attributes_digest == old.attributes_digest
        && current.sparse_digest == old.sparse_digest
        && current.filesystem_capabilities_digest == old.filesystem_capabilities_digest
        && current.captured_at == old.captured_at
        && current.coverage == old.coverage
}

/// A restart may prove an unsigned commit by reconstructing every durable
/// intent field from the immutable commit object. Signed intents intentionally
/// retain only a key-reference digest in the preview, so they remain
/// `NeedsInspection` rather than guessing a key identity.
fn commit_intent_matches_preview(
    repository_root: &Path,
    head: &tracedecay_domain::GitOidV1,
    preview: &GitIndexPreviewV1,
) -> bool {
    let Some(expected_digest) = preview.commit_intent_digest.as_ref() else {
        return false;
    };
    let Ok(signature_output) = read_git_command(repository_root)
        .args(["show", "-s", "--format=%G?", head.as_str()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    let Ok(signature_status) = String::from_utf8(signature_output.stdout) else {
        return false;
    };
    if !signature_output.status.success() || signature_status.trim() != "N" {
        return false;
    }
    let output = read_git_command(repository_root)
        .args([
            "show",
            "-s",
            "--format=%an%x00%ae%x00%at%x00%cn%x00%ce%x00%ct%x00%B",
            head.as_str(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok();
    let Some(output) = output.filter(|output| output.status.success()) else {
        return false;
    };
    let Ok(text) = String::from_utf8(output.stdout) else {
        return false;
    };
    let mut parts = text.splitn(7, '\0');
    let Some(author_name) = parts.next() else {
        return false;
    };
    let Some(author_email) = parts.next() else {
        return false;
    };
    let Some(author_seconds) = parts.next().and_then(|value| value.parse::<i64>().ok()) else {
        return false;
    };
    let Some(committer_name) = parts.next() else {
        return false;
    };
    let Some(committer_email) = parts.next() else {
        return false;
    };
    let Some(committer_seconds) = parts.next().and_then(|value| value.parse::<i64>().ok()) else {
        return false;
    };
    let Some(message) = parts.next() else {
        return false;
    };
    let Some(author_micros) = author_seconds.checked_mul(1_000_000) else {
        return false;
    };
    let Some(committer_micros) = committer_seconds.checked_mul(1_000_000) else {
        return false;
    };
    GitIndexCommitIntentV1::new(
        message.to_owned(),
        GitCommitIdentityV1 {
            name: author_name.to_owned(),
            email: author_email.to_owned(),
            at: UtcMicros(author_micros),
        },
        GitCommitIdentityV1 {
            name: committer_name.to_owned(),
            email: committer_email.to_owned(),
            at: UtcMicros(committer_micros),
        },
        GitIndexSigningPolicyV1::UnsignedPermitted,
    )
    .and_then(|intent| intent.compute_digest())
    .is_ok_and(|digest| digest == *expected_digest)
}

fn read_git_value(repository_root: &Path, expression: &str) -> Option<String> {
    let output = read_git_command(repository_root)
        .args(["rev-parse", "--verify", expression])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

/// Fixed native implementation used by the daemon coordinator. It accepts
/// only preview-bound material and rejects a cache miss after restart rather
/// than reconstructing or guessing a patch.
pub(crate) struct FixedDaemonGitIndexExecutor<A> {
    assembler: A,
    previews: Mutex<BTreeMap<GitIndexPreviewId, MaterializedGitIndexPreview>>,
}

impl<A> FixedDaemonGitIndexExecutor<A> {
    pub(crate) fn new(assembler: A) -> Self {
        Self {
            assembler,
            previews: Mutex::new(BTreeMap::new()),
        }
    }
}

impl<A> GitIndexNativeExecutor for FixedDaemonGitIndexExecutor<A>
where
    A: GitIndexPreviewAssembler,
{
    fn preview(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<GitIndexPreviewPortResultV1, GitIndexTransactionPortError> {
        let materialized = self.assembler.materialize(request)?;
        materialized
            .preview
            .validate()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let result = GitIndexPreviewPortResultV1 {
            preview: materialized.preview.clone(),
            execution: materialized.execution.clone(),
        };
        let mut previews = self
            .previews
            .lock()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?;
        previews.retain(|_, cached| !cached.preview.is_expired_at(request.observed_at));
        match previews.get(&materialized.preview.preview_id) {
            Some(existing)
                if existing.preview.preview_digest != materialized.preview.preview_digest =>
            {
                return Err(GitIndexTransactionPortError::StalePreview);
            }
            None if materialized.preview.disposition.is_applicable() => {
                previews.insert(materialized.preview.preview_id.clone(), materialized);
            }
            Some(_) | None => {}
        }
        Ok(result)
    }

    fn apply(
        &self,
        transaction_id: &GitIndexTransactionId,
        preview: &GitIndexPreviewV1,
        request: &GitIndexApplyRequestV1,
    ) -> Result<NativeGitIndexApplyOutcomeV1, GitIndexTransactionPortError> {
        if request.validate().is_err() {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        }
        let Ok(mut previews) = self.previews.lock() else {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        };
        let scope = request.context.scope();
        let Some(cached) = previews.get(&request.preview_id) else {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        };
        if scope.project_id != cached.preview.repository_snapshot.project_id
            || scope.repository_id != cached.preview.repository_snapshot.repository_id
            || cached.preview.repository_snapshot.worktree_id.as_ref() != Some(&scope.worktree_id)
        {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        }
        // Commit messages, identities, and signing keys are process-local
        // ephemeral material. An authorized apply attempt consumes the one-shot
        // materialization before preview validation or native work so no stale
        // or terminal attempt retains plaintext.
        let Some(materialized) = previews.remove(&request.preview_id) else {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        };
        drop(previews);
        if request.validate_for_preview(preview).is_err() {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        }
        if materialized.preview != *preview {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        }
        let Ok(mut index_lock) = materialized.runner.acquire_index_lock() else {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        };
        let Ok(current) = self.assembler.capture_current(&materialized, &index_lock) else {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        };
        let Ok(current_digest) = GitIndexPreviewV1::repository_snapshot_digest(&current) else {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        };
        if current != preview.repository_snapshot
            || current_digest != preview.repository_snapshot_digest
        {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        }
        let Ok(current_patches) = self.assembler.revalidate_patches(&materialized) else {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        };
        if current_patches.len() != materialized.patches.len() {
            return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
        }

        let created_commit = match preview.operation {
            GitIndexTransactionOperationV1::StageHunks => {
                if let Err(error) =
                    materialized
                        .runner
                        .stage_hunks(&mut index_lock, preview, &current_patches)
                {
                    return Ok(classify_native_failure(&error));
                }
                None
            }
            GitIndexTransactionOperationV1::UnstageHunks => {
                if let Err(error) =
                    materialized
                        .runner
                        .unstage_hunks(&mut index_lock, preview, &current_patches)
                {
                    return Ok(classify_native_failure(&error));
                }
                None
            }
            GitIndexTransactionOperationV1::CommitIndex => {
                let Some(intent) = materialized.commit_intent.as_ref() else {
                    return Ok(NativeGitIndexApplyOutcomeV1::ProvenNoMutation);
                };
                match materialized
                    .runner
                    .commit_index(&index_lock, preview, intent)
                {
                    Ok(commit) => Some(commit),
                    Err(error) => return Ok(classify_native_failure(&error)),
                }
            }
        };
        drop(index_lock);
        match self.assembler.finalize(
            &materialized,
            transaction_id,
            request,
            created_commit.as_ref(),
        ) {
            Ok(result) => Ok(NativeGitIndexApplyOutcomeV1::Completed(Box::new(result))),
            // Final observation happens after the native publication/commit
            // operation, so failing to observe it is itself ambiguous.
            Err(_) => Ok(NativeGitIndexApplyOutcomeV1::CommitBoundaryUnknown),
        }
    }

    fn discard_preview(&self, preview_id: &GitIndexPreviewId) {
        self.previews
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(preview_id);
    }
}

impl<A> GitIndexRecoveryExecutor for FixedDaemonGitIndexExecutor<A>
where
    A: GitIndexPreviewAssembler,
{
    fn reconcile(
        &self,
        record: &GitIndexTransactionRecordV1,
    ) -> Result<GitIndexTransactionReceiptV1, GitIndexRecoveryError> {
        self.assembler.reconcile(record)
    }
}

#[allow(clippy::needless_pass_by_value)]
fn map_native_error(error: NativeGitIndexError) -> GitIndexTransactionPortError {
    match error {
        NativeGitIndexError::IndexLocked
        | NativeGitIndexError::PartialHunkSelectionUnsupported
        | NativeGitIndexError::CommitStateUnsupported
        | NativeGitIndexError::EmptyIndexCommit
        | NativeGitIndexError::UnsupportedHookPolicy => GitIndexTransactionPortError::Unsupported,
        NativeGitIndexError::PatchDoesNotMatchHunk
        | NativeGitIndexError::CandidateTreeMismatch
        | NativeGitIndexError::CommitIntentMismatch
        | NativeGitIndexError::StaleRepositoryState => GitIndexTransactionPortError::StalePreview,
        // Native output we could not interpret is not evidence that the
        // caller's snapshot moved. Reporting it as staleness told every caller
        // to recapture and retry a request that will fail identically, and it
        // made an adapter defect indistinguishable from ordinary contention.
        NativeGitIndexError::MalformedOutput { .. } | NativeGitIndexError::Domain(_) => {
            GitIndexTransactionPortError::NativeFailure
        }
        NativeGitIndexError::RepositoryUnavailable(_)
        | NativeGitIndexError::GitFailed { .. }
        | NativeGitIndexError::Io(_)
        | NativeGitIndexError::CommitBoundaryUnknown { .. } => {
            GitIndexTransactionPortError::NeedsInspection
        }
    }
}

fn classify_native_failure(error: &NativeGitIndexError) -> NativeGitIndexApplyOutcomeV1 {
    if error.is_commit_boundary_unknown() {
        NativeGitIndexApplyOutcomeV1::CommitBoundaryUnknown
    } else {
        NativeGitIndexApplyOutcomeV1::ProvenNoMutation
    }
}

#[cfg(any(test, feature = "test-transport"))]
#[cfg_attr(not(unix), allow(dead_code))] // exercised only by unix-only daemon tests
pub(crate) fn capture_exact_snapshot_for_test(
    repository_root: &std::path::Path,
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    captured_at: UtcMicros,
) -> crate::errors::Result<RepositoryStateSnapshotV1> {
    // Same canonical root the daemon owner mounts; alias paths must not mint a
    // divergent snapshot that later fails exact preview CAS.
    let repository_root = super::canonicalize_repository_root(repository_root)?;
    let assembler = NativeGitIndexPreviewAssembler::new(
        &repository_root,
        project_id,
        repository_id,
        worktree_id,
    );
    let runner = FixedGitIndexRunner::new(&repository_root).map_err(test_snapshot_error)?;
    let status = assembler
        .read_authority()
        .status()
        .map_err(test_snapshot_error)?;
    let lock = runner.acquire_index_lock().map_err(test_snapshot_error)?;
    let tree = runner
        .index_tree_under_lock(&lock)
        .map_err(test_snapshot_error)?;
    let placeholder = RepositoryStateSnapshotV1::new(
        assembler.project_id.clone(),
        assembler.repository_id.clone(),
        Some(assembler.worktree_id.clone()),
        1,
        tree.format(),
        status.head,
        RepositoryIndexSnapshotV1 {
            checksum: canonical_sha256(&b"placeholder".as_slice()).map_err(test_snapshot_error)?,
            tree_id: Some(tree),
            state: RepositoryIndexStateV1::Clean,
            unmerged_stage_digest: None,
        },
        RepositoryWorkingTreeSnapshotV1 {
            state: RepositoryWorkingTreeStateV1::Clean,
            tracked_digest: canonical_sha256(&b"placeholder".as_slice())
                .map_err(test_snapshot_error)?,
            untracked_name_digest: None,
            ignored_collision_digest: None,
        },
        tracedecay_domain::GitOperationStateV1::None,
        None,
        None,
        None,
        None,
        captured_at,
        tracedecay_domain::GitCoverageV1::complete(),
    )
    .map_err(test_snapshot_error)?;
    assembler
        .capture_snapshot(&placeholder, &runner, &lock)
        .map_err(test_snapshot_error)
}

#[cfg(any(test, feature = "test-transport"))]
fn test_snapshot_error(error: impl std::fmt::Display) -> crate::errors::TraceDecayError {
    crate::errors::TraceDecayError::Config {
        message: format!("failed to capture exact Git test snapshot: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;

    use tempfile::TempDir;
    use tracedecay_application::{
        AuthorityReceipt, CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot,
        Deadline, DisclosureClass, PolicyDecisionRef, RequestContext, RequestId, ResolvedScope,
    };
    use tracedecay_domain::{
        ActorId, ComponentVersion, GitCommitIdentityV1, GitCoverageV1, GitIndexIdempotencyKey,
        GitIndexJournalPhaseV1, GitIndexSigningPolicyV1, GitIndexTransactionId,
        GitIndexTransactionJournalV1, GitObjectFormatV1, GitOperationStateV1, RefId,
    };
    use tracedecay_store::GitIndexTransactionRecordV1;
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;

    fn git(repository: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(repository)
            .args(args)
            .status()
            .expect("Git command starts");
        assert!(status.success(), "git {args:?}");
    }

    fn git_value(repository: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(repository)
            .args(args)
            .output()
            .expect("Git command starts");
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout)
            .expect("Git output is UTF-8")
            .trim()
            .to_owned()
    }

    fn repository_fixture() -> (TempDir, NativeGitIndexPreviewAssembler, FixedGitIndexRunner) {
        let directory = tempfile::tempdir().expect("temporary repository");
        git(directory.path(), &["init", "--quiet"]);
        git(
            directory.path(),
            &["config", "user.name", "TraceDecay Test"],
        );
        git(
            directory.path(),
            &["config", "user.email", "tracedecay@example.com"],
        );
        fs::write(directory.path().join("packet.txt"), "before\n").expect("write base file");
        git(directory.path(), &["add", "packet.txt"]);
        git(directory.path(), &["commit", "--quiet", "-m", "base"]);
        let assembler = NativeGitIndexPreviewAssembler::new(
            directory.path(),
            ProjectId::new("project.fixture").expect("project id"),
            RepositoryId::new("repository.fixture").expect("repository id"),
            WorktreeId::new("worktree.fixture").expect("worktree id"),
        );
        let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
        (directory, assembler, runner)
    }

    fn exact_snapshot(
        assembler: &NativeGitIndexPreviewAssembler,
        runner: &FixedGitIndexRunner,
    ) -> RepositoryStateSnapshotV1 {
        let status = assembler.read_authority().status().expect("native status");
        let lock = runner.acquire_index_lock().expect("snapshot index lock");
        let tree = runner.index_tree_under_lock(&lock).expect("index tree");
        let placeholder = RepositoryStateSnapshotV1::new(
            assembler.project_id.clone(),
            assembler.repository_id.clone(),
            Some(assembler.worktree_id.clone()),
            1,
            GitObjectFormatV1::Sha1,
            status.head,
            RepositoryIndexSnapshotV1 {
                checksum: canonical_sha256(&b"placeholder".as_slice()).expect("digest"),
                tree_id: Some(tree),
                state: RepositoryIndexStateV1::Clean,
                unmerged_stage_digest: None,
            },
            RepositoryWorkingTreeSnapshotV1 {
                state: RepositoryWorkingTreeStateV1::Clean,
                tracked_digest: canonical_sha256(&b"placeholder".as_slice()).expect("digest"),
                untracked_name_digest: None,
                ignored_collision_digest: None,
            },
            GitOperationStateV1::None,
            None,
            None,
            None,
            None,
            UtcMicros(1),
            GitCoverageV1::complete(),
        )
        .expect("placeholder snapshot");
        assembler
            .capture_snapshot(&placeholder, runner, &lock)
            .expect("exact native snapshot")
    }

    fn hunk_preview(
        assembler: &NativeGitIndexPreviewAssembler,
        runner: &FixedGitIndexRunner,
        operation: GitIndexTransactionOperationV1,
        scope: GitDiffScopeV1,
        preview_id: &str,
    ) -> (GitIndexPreviewV1, Vec<ValidatedIndexPatch>) {
        let snapshot = exact_snapshot(assembler, runner);
        let snapshot_digest =
            GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
        let references = assembler
            .read_authority()
            .hunk_refs(&scope, preview_id, &snapshot_digest)
            .expect("current hunk refs");
        assert_eq!(references.len(), 1, "fixture has one hunk");
        let patches = assembler
            .materialize_selected_patches(operation, preview_id, &references, &snapshot_digest)
            .expect("materialized patches");
        let lock = runner.acquire_index_lock().expect("preview index lock");
        let candidate = runner
            .preview_candidate_tree_under_lock(
                &lock,
                &patches,
                operation == GitIndexTransactionOperationV1::UnstageHunks,
            )
            .expect("candidate tree");
        let preview = GitIndexPreviewV1::new(
            GitIndexPreviewId::new(preview_id).expect("preview id"),
            operation,
            snapshot,
            snapshot_digest,
            references,
            Some(candidate),
            GitIndexPreviewDispositionV1::Applicable,
            UtcMicros(2),
            UtcMicros(3),
        )
        .expect("hunk preview");
        (preview, patches)
    }

    fn commit_preview(
        assembler: &NativeGitIndexPreviewAssembler,
        runner: &FixedGitIndexRunner,
        preview_id: &str,
        intent: &GitIndexCommitIntentV1,
    ) -> GitIndexPreviewV1 {
        let snapshot = exact_snapshot(assembler, runner);
        let snapshot_digest =
            GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
        GitIndexPreviewV1::new_with_commit_intent(
            GitIndexPreviewId::new(preview_id).expect("preview id"),
            GitIndexTransactionOperationV1::CommitIndex,
            snapshot.clone(),
            snapshot_digest,
            Vec::new(),
            snapshot.index.tree_id,
            Some(intent),
            GitIndexPreviewDispositionV1::Applicable,
            UtcMicros(2),
            UtcMicros(3),
        )
        .expect("commit preview")
    }

    fn commit_intent(message: &str) -> GitIndexCommitIntentV1 {
        let identity = GitCommitIdentityV1 {
            name: "TraceDecay Test".to_owned(),
            email: "tracedecay@example.com".to_owned(),
            at: UtcMicros(1_000_000),
        };
        GitIndexCommitIntentV1::new(
            message.to_owned(),
            identity.clone(),
            identity,
            GitIndexSigningPolicyV1::UnsignedPermitted,
        )
        .expect("commit intent")
    }

    fn commit_request(
        snapshot: RepositoryStateSnapshotV1,
        intent: GitIndexCommitIntentV1,
        preview_id: &str,
    ) -> GitIndexPreviewRequestV1 {
        let capability_id = CapabilityId::new("capability.git.commit-index").expect("capability");
        let use_case_id = UseCaseId::new("use-case.git.commit-index").expect("use case");
        let GitHeadStateV1::Attached { branch, .. } = &snapshot.head else {
            panic!("fixture has attached HEAD");
        };
        let scope = ResolvedScope::new(
            snapshot.project_id.clone(),
            snapshot.repository_id.clone(),
            snapshot.worktree_id.clone().expect("fixture worktree"),
            Some(RefId::new(branch.clone()).expect("fixture ref")),
        )
        .expect("scope");
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.git-preview.fixture").expect("grant"),
            1,
            canonical_sha256(&"git preview grant").expect("grant digest"),
            ActorId::new("actor.git-preview.issuer").expect("issuer"),
            UtcMicros(1),
            UtcMicros(1_000),
            scope.clone(),
            BTreeSet::from([capability_id.clone()]),
            BTreeSet::from([use_case_id.clone()]),
            DisclosureClass::Sensitive,
        )
        .expect("grant");
        let context = RequestContext::new(
            ActorId::new("actor.git-preview.requester").expect("requester"),
            scope,
            grant,
            RequestId::new(format!("request.{preview_id}")).expect("request"),
            Deadline::new(UtcMicros(500)).expect("deadline"),
            CancellationContext::active(format!("cancel.{preview_id}")).expect("cancellation"),
        )
        .expect("context");
        let authority = AuthorityReceipt::from_context(
            &context,
            PolicyDecisionRef::new(
                "policy.git-preview.fixture",
                1,
                canonical_sha256(&"git preview policy").expect("policy digest"),
                ComponentVersion::new("policy.git-preview.v1").expect("policy version"),
            )
            .expect("policy"),
            UtcMicros(2),
        )
        .expect("authority");
        GitIndexPreviewRequestV1 {
            context,
            authority,
            binding: tracedecay_application::GitIndexOperationBindingV1 {
                capability_id,
                use_case_id,
                operation: GitIndexTransactionOperationV1::CommitIndex,
            },
            preview_id: GitIndexPreviewId::new(preview_id).expect("preview id"),
            repository_snapshot: snapshot,
            selected_hunks: Vec::new(),
            commit_intent: Some(intent),
            observed_at: UtcMicros(10),
        }
    }

    fn object_file_count(path: &Path) -> usize {
        fs::read_dir(path)
            .expect("object directory")
            .filter_map(Result::ok)
            .map(|entry| {
                if entry.path().is_dir() {
                    object_file_count(&entry.path())
                } else {
                    1
                }
            })
            .sum()
    }

    fn recovery_record(
        preview: &GitIndexPreviewV1,
        phase: GitIndexJournalPhaseV1,
    ) -> GitIndexTransactionRecordV1 {
        let transaction_id =
            GitIndexTransactionId::new(format!("git-index-transaction.recovery.{}", phase as u8))
                .expect("transaction id");
        let mut journal =
            GitIndexTransactionJournalV1::prepared(transaction_id, preview, UtcMicros(10))
                .expect("prepared journal");
        for successor in match phase {
            GitIndexJournalPhaseV1::Prepared => &[][..],
            GitIndexJournalPhaseV1::NativeApplyStarted => {
                &[GitIndexJournalPhaseV1::NativeApplyStarted][..]
            }
            GitIndexJournalPhaseV1::IndexCommitted => &[
                GitIndexJournalPhaseV1::NativeApplyStarted,
                GitIndexJournalPhaseV1::IndexCommitted,
            ][..],
            GitIndexJournalPhaseV1::RefCommitted => &[
                GitIndexJournalPhaseV1::NativeApplyStarted,
                GitIndexJournalPhaseV1::IndexCommitted,
                GitIndexJournalPhaseV1::RefCommitted,
            ][..],
            GitIndexJournalPhaseV1::Verifying => &[
                GitIndexJournalPhaseV1::NativeApplyStarted,
                GitIndexJournalPhaseV1::IndexCommitted,
                GitIndexJournalPhaseV1::Verifying,
            ][..],
            GitIndexJournalPhaseV1::Committed
            | GitIndexJournalPhaseV1::AbortedNoChange
            | GitIndexJournalPhaseV1::NeedsInspection => {
                panic!("fixture only constructs non-terminal recovery phases")
            }
        } {
            journal
                .advance(*successor, UtcMicros(journal.updated_at.0 + 1))
                .expect("legal phase transition");
        }
        GitIndexTransactionRecordV1 {
            idempotency_key: GitIndexIdempotencyKey::new(format!(
                "idempotency.recovery.{}",
                phase as u8
            ))
            .expect("idempotency key"),
            input_digest: canonical_sha256(&("recovery", phase as u8)).expect("input digest"),
            preview: preview.clone(),
            journal,
            terminal_receipt: None,
        }
    }

    #[test]
    fn real_repository_preview_uses_quarantined_index_and_exact_hunk_packet() {
        let directory = tempfile::tempdir().expect("temporary repository");
        git(directory.path(), &["init", "--quiet"]);
        git(
            directory.path(),
            &["config", "user.name", "TraceDecay Test"],
        );
        git(
            directory.path(),
            &["config", "user.email", "tracedecay@example.com"],
        );
        fs::write(directory.path().join("packet.txt"), "before\n").expect("write base file");
        git(directory.path(), &["add", "packet.txt"]);
        git(directory.path(), &["commit", "--quiet", "-m", "base"]);
        fs::write(directory.path().join("packet.txt"), "after\n").expect("write changed file");

        let repository_id = RepositoryId::new("repository.fixture").expect("repository id");
        let worktree_id = WorktreeId::new("worktree.fixture").expect("worktree id");
        let intelligence = NativeGitIntelligence::new(directory.path(), repository_id, worktree_id);
        let snapshot_digest =
            ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).expect("snapshot digest");
        let hunk = intelligence
            .hunk_refs(
                &GitDiffScopeV1::WorkingTree,
                "git-index-preview.fixture",
                &snapshot_digest,
            )
            .expect("mint current hunk")
            .into_iter()
            .next()
            .expect("one hunk");
        let mut rename_hunk = hunk.clone();
        rename_hunk.original_path = Some("packet-old.txt".to_owned());
        let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
        assert_eq!(
            unsupported_hunk_selection(
                &[rename_hunk],
                &runner,
                &intelligence,
                GitIndexTransactionOperationV1::StageHunks,
            ),
            Some(GitIndexUnsupportedStateV1::RenameOrCopy),
            "rename/copy hunks remain explicit read-only previews"
        );
        let patch = extract_patch(directory.path(), &GitDiffScopeV1::WorkingTree, &hunk)
            .expect("extract exact packet");
        let patch = ValidatedIndexPatch::new(hunk, patch).expect("validate exact packet");
        let old_tree = runner.write_tree().expect("old index tree");
        let candidate = runner
            .preview_candidate_tree(&[patch], false)
            .expect("quarantined candidate tree");

        assert_ne!(candidate, old_tree);
        assert_eq!(
            runner.write_tree().expect("real index remains unchanged"),
            old_tree
        );
    }

    #[test]
    fn preview_preflight_classifies_repository_and_commit_blockers_without_mutation() {
        let bare = tempfile::tempdir().expect("bare repository");
        git(bare.path(), &["init", "--bare", "--quiet"]);
        let bare_runner = FixedGitIndexRunner::new(bare.path()).expect("bare runner");
        assert!(bare_runner.is_bare_repository().expect("bare probe"));
        assert!(supported_object_format("sha1"));
        assert!(supported_object_format("sha256"));
        assert!(!supported_object_format("sha512"));

        let (directory, _assembler, runner) = repository_fixture();
        let before = runner.index_bytes().expect("index before probes");
        fs::write(runner.index_lock_path(), b"external owner").expect("external lock");
        assert!(matches!(
            runner.ensure_index_unlocked(),
            Err(NativeGitIndexError::IndexLocked)
        ));
        fs::remove_file(runner.index_lock_path()).expect("remove fixture lock");

        let hook = directory.path().join(".git/hooks/pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 0\n").expect("write hook");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&hook).expect("hook metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&hook, permissions).expect("executable hook");
            assert!(
                runner
                    .has_applicable_commit_hooks()
                    .expect("hook classification")
            );
        }
        assert!(
            !runner
                .signing_key_available("tracedecay-missing-signing-key")
                .expect("missing signing key classification")
        );
        assert_eq!(runner.index_bytes().expect("index after probes"), before);
    }

    #[test]
    fn native_blockers_never_mint_a_preview_from_stale_caller_state() {
        let (directory, assembler, runner) = repository_fixture();
        let stale = commit_request(
            exact_snapshot(&assembler, &runner),
            commit_intent("stale preview\n"),
            "git-index-preview.stale-native-blocker",
        );
        fs::write(directory.path().join("packet.txt"), "changed\n").expect("stale worktree");
        fs::write(runner.index_lock_path(), b"external owner").expect("external lock");
        assert!(matches!(
            assembler.materialize(&stale),
            Err(GitIndexTransactionPortError::Unsupported)
        ));
        fs::remove_file(runner.index_lock_path()).expect("remove external lock");

        let hook = directory.path().join(".git/hooks/pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 0\n").expect("write hook");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&hook).expect("hook metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&hook, permissions).expect("executable hook");
        }
        assert!(matches!(
            assembler.materialize(&stale),
            Err(GitIndexTransactionPortError::StalePreview)
        ));

        let fresh_snapshot = exact_snapshot(&assembler, &runner);
        let fresh = commit_request(
            fresh_snapshot.clone(),
            commit_intent("fresh preview\n"),
            "git-index-preview.fresh-native-blocker",
        );
        let materialized = assembler
            .materialize(&fresh)
            .expect("verified unsupported preview");
        assert_eq!(materialized.preview.repository_snapshot, fresh_snapshot);
        assert_eq!(
            materialized.preview.disposition,
            GitIndexPreviewDispositionV1::Unsupported(
                GitIndexUnsupportedStateV1::ApplicableCommitHooks
            )
        );
    }

    #[test]
    fn preview_classifies_partial_binary_filter_and_special_mode_hunks() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\nsecond\n")
            .expect("change text file");
        assert_eq!(
            unsupported_path_state(
                &runner,
                &assembler.read_authority(),
                GitIndexTransactionOperationV1::StageHunks,
                "missing.txt"
            ),
            Some(GitIndexUnsupportedStateV1::UnreadableWorkingTree)
        );
        assert_eq!(
            unsupported_path_state(
                &runner,
                &assembler.read_authority(),
                GitIndexTransactionOperationV1::UnstageHunks,
                "missing.txt"
            ),
            Some(GitIndexUnsupportedStateV1::UnreadableIndex)
        );
        let snapshot = exact_snapshot(&assembler, &runner);
        let snapshot_digest =
            GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
        let mut hunk = assembler
            .read_authority()
            .hunk_refs(
                &GitDiffScopeV1::WorkingTree,
                "git-index-preview.partial",
                &snapshot_digest,
            )
            .expect("text hunk")
            .remove(0);
        let full_hunk = hunk.clone();
        hunk.selected_line_bitmap = vec![1];
        assert_eq!(
            unsupported_hunk_selection(
                &[hunk],
                &runner,
                &assembler.read_authority(),
                GitIndexTransactionOperationV1::StageHunks
            ),
            Some(GitIndexUnsupportedStateV1::PartialHunkSelection)
        );

        fs::write(directory.path().join("packet.txt"), [0_u8, 1, 2, 3]).expect("binary change");
        assert_eq!(
            unsupported_path_state(
                &runner,
                &assembler.read_authority(),
                GitIndexTransactionOperationV1::StageHunks,
                "packet.txt"
            ),
            Some(GitIndexUnsupportedStateV1::BinaryHunk)
        );

        fs::write(
            directory.path().join(".gitattributes"),
            "*.txt text eol=lf\n",
        )
        .expect("attributes");
        fs::write(directory.path().join("packet.txt"), "filtered\n").expect("filtered change");
        assert_eq!(
            unsupported_path_state(
                &runner,
                &assembler.read_authority(),
                GitIndexTransactionOperationV1::StageHunks,
                "packet.txt"
            ),
            Some(GitIndexUnsupportedStateV1::FiltersOrEndOfLine)
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            fs::remove_file(directory.path().join(".gitattributes")).expect("remove attributes");
            fs::write(directory.path().join("mode.txt"), "mode\n").expect("mode file");
            git(directory.path(), &["add", "mode.txt"]);
            git(directory.path(), &["commit", "--quiet", "-m", "mode base"]);
            let mut permissions = fs::metadata(directory.path().join("mode.txt"))
                .expect("mode metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(directory.path().join("mode.txt"), permissions)
                .expect("mode change");
            assert_eq!(
                unsupported_path_state(
                    &runner,
                    &assembler.read_authority(),
                    GitIndexTransactionOperationV1::StageHunks,
                    "mode.txt"
                ),
                Some(GitIndexUnsupportedStateV1::FileModeOnly)
            );

            symlink("packet.txt", directory.path().join("link.txt")).expect("symlink");
            git(directory.path(), &["add", "link.txt"]);
            assert_eq!(
                unsupported_path_state(
                    &runner,
                    &assembler.read_authority(),
                    GitIndexTransactionOperationV1::UnstageHunks,
                    "link.txt"
                ),
                Some(GitIndexUnsupportedStateV1::Symlink)
            );
        }

        let mut submodule_hunk = full_hunk;
        submodule_hunk.expected_index_entry.mode = Some(
            tracedecay_domain::GitFileModeV1::new(tracedecay_domain::GitFileModeV1::GITLINK)
                .expect("gitlink mode"),
        );
        assert_eq!(
            unsupported_hunk_selection(
                &[submodule_hunk],
                &runner,
                &assembler.read_authority(),
                GitIndexTransactionOperationV1::StageHunks,
            ),
            Some(GitIndexUnsupportedStateV1::Submodule)
        );
    }

    #[test]
    fn stage_unstage_and_replay_are_atomic_against_the_real_index() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        let original_tree = runner.write_tree().expect("original tree");
        let (stage, stage_patches) = hunk_preview(
            &assembler,
            &runner,
            GitIndexTransactionOperationV1::StageHunks,
            GitDiffScopeV1::WorkingTree,
            "git-index-preview.stage",
        );
        let mut lock = runner.acquire_index_lock().expect("stage lock");
        runner
            .stage_hunks(&mut lock, &stage, &stage_patches)
            .expect("stage exact hunk");
        drop(lock);
        assert_eq!(
            runner.write_tree().expect("staged tree"),
            stage.candidate_index_tree.clone().expect("candidate tree")
        );

        let once = runner.index_bytes().expect("index after first apply");
        let mut replay_lock = runner.acquire_index_lock().expect("replay lock");
        assert!(matches!(
            runner.stage_hunks(&mut replay_lock, &stage, &stage_patches),
            Err(NativeGitIndexError::StaleRepositoryState)
        ));
        drop(replay_lock);
        assert_eq!(runner.index_bytes().expect("index after replay"), once);

        let (unstage, unstage_patches) = hunk_preview(
            &assembler,
            &runner,
            GitIndexTransactionOperationV1::UnstageHunks,
            GitDiffScopeV1::Staged,
            "git-index-preview.unstage",
        );
        let mut lock = runner.acquire_index_lock().expect("unstage lock");
        runner
            .unstage_hunks(&mut lock, &unstage, &unstage_patches)
            .expect("unstage exact hunk");
        drop(lock);
        assert_eq!(runner.write_tree().expect("unstaged tree"), original_tree);
    }

    #[test]
    fn recovery_requires_post_boundary_phase_evidence_for_candidate_trees() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        let (preview, patches) = hunk_preview(
            &assembler,
            &runner,
            GitIndexTransactionOperationV1::StageHunks,
            GitDiffScopeV1::WorkingTree,
            "git-index-preview.recovery-phase",
        );
        let mut lock = runner.acquire_index_lock().expect("index lock");
        runner
            .stage_hunks(&mut lock, &preview, &patches)
            .expect("publish candidate index");
        drop(lock);

        let lock = runner.acquire_index_lock().expect("recovery snapshot lock");
        let current = assembler
            .capture_snapshot(&preview.repository_snapshot, &runner, &lock)
            .expect("recovery snapshot");
        drop(lock);
        assert_eq!(current.index.tree_id, preview.candidate_index_tree);
        assert_eq!(current.head, preview.repository_snapshot.head);
        assert_eq!(current.refs_digest, preview.repository_snapshot.refs_digest);
        assert!(same_stable_native_evidence(
            &current,
            &preview.repository_snapshot
        ));

        let unproven = recovery_record(&preview, GitIndexJournalPhaseV1::NativeApplyStarted);
        assert_eq!(
            assembler
                .reconcile(&unproven)
                .expect("reconcile unproven candidate")
                .outcome,
            GitIndexReceiptOutcomeV1::NeedsInspection,
            "candidate-tree equality before an fsynced index phase can be external coincidence"
        );

        let proven = recovery_record(&preview, GitIndexJournalPhaseV1::IndexCommitted);
        assert_eq!(
            assembler
                .reconcile(&proven)
                .expect("reconcile phase-proven candidate")
                .outcome,
            GitIndexReceiptOutcomeV1::Committed
        );
    }

    #[test]
    fn recovery_rejects_candidate_tree_when_the_previewed_ref_drifted() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        let (preview, patches) = hunk_preview(
            &assembler,
            &runner,
            GitIndexTransactionOperationV1::StageHunks,
            GitDiffScopeV1::WorkingTree,
            "git-index-preview.recovery-ref-drift",
        );
        let mut lock = runner.acquire_index_lock().expect("index lock");
        runner
            .stage_hunks(&mut lock, &preview, &patches)
            .expect("publish candidate index");
        drop(lock);
        git(
            directory.path(),
            &["commit", "--quiet", "-m", "external ref drift"],
        );
        let lock = runner.acquire_index_lock().expect("drift snapshot lock");
        let current = assembler
            .capture_snapshot(&preview.repository_snapshot, &runner, &lock)
            .expect("drift snapshot");
        drop(lock);
        assert!(
            !live_result_matches_preview(directory.path(), &preview, &current, None),
            "a hunk mutation must not report success after HEAD/ref drift"
        );

        let record = recovery_record(&preview, GitIndexJournalPhaseV1::IndexCommitted);
        assert_eq!(
            assembler
                .reconcile(&record)
                .expect("reconcile ref drift")
                .outcome,
            GitIndexReceiptOutcomeV1::NeedsInspection
        );
    }

    #[test]
    fn recovery_rejects_commit_with_matching_tree_but_wrong_durable_intent() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        git(directory.path(), &["add", "packet.txt"]);
        let expected_intent = commit_intent("expected transaction message\n");
        let preview = commit_preview(
            &assembler,
            &runner,
            "git-index-preview.recovery-intent",
            &expected_intent,
        );
        git(
            directory.path(),
            &["commit", "--quiet", "-m", "different external message"],
        );

        let record = recovery_record(&preview, GitIndexJournalPhaseV1::RefCommitted);
        assert_eq!(
            assembler
                .reconcile(&record)
                .expect("reconcile wrong intent")
                .outcome,
            GitIndexReceiptOutcomeV1::NeedsInspection
        );
    }

    #[test]
    fn recovery_commits_when_git_normalizes_unaligned_intent_timestamps() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        git(directory.path(), &["add", "packet.txt"]);
        let identity = GitCommitIdentityV1 {
            name: "TraceDecay Test".to_owned(),
            email: "tracedecay@example.com".to_owned(),
            at: UtcMicros(1_234_567),
        };
        let intent = GitIndexCommitIntentV1::new(
            "unaligned timestamp transaction\n".to_owned(),
            identity.clone(),
            identity,
            GitIndexSigningPolicyV1::UnsignedPermitted,
        )
        .expect("commit intent");
        let preview = commit_preview(
            &assembler,
            &runner,
            "git-index-preview.recovery-unaligned-timestamp",
            &intent,
        );
        let lock = runner.acquire_index_lock().expect("commit lock");
        runner
            .commit_index(&lock, &preview, &intent)
            .expect("commit exact index");
        drop(lock);

        let record = recovery_record(&preview, GitIndexJournalPhaseV1::RefCommitted);
        assert_eq!(
            assembler
                .reconcile(&record)
                .expect("reconcile normalized intent")
                .outcome,
            GitIndexReceiptOutcomeV1::Committed
        );
    }

    #[test]
    fn stale_worktree_hunk_and_index_lock_leave_index_unchanged() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        let (preview, patches) = hunk_preview(
            &assembler,
            &runner,
            GitIndexTransactionOperationV1::StageHunks,
            GitDiffScopeV1::WorkingTree,
            "git-index-preview.stale-hunk",
        );
        let before = runner.index_bytes().expect("initial index");
        fs::write(directory.path().join("packet.txt"), "changed again\n").expect("stale worktree");
        let mut lock = runner.acquire_index_lock().expect("apply lock");
        assert!(matches!(
            runner.stage_hunks(&mut lock, &preview, &patches),
            Err(NativeGitIndexError::StaleRepositoryState)
        ));
        drop(lock);
        assert_eq!(
            runner.index_bytes().expect("index after stale hunk"),
            before
        );

        fs::write(runner.index_lock_path(), b"external owner").expect("external index lock");
        assert!(matches!(
            runner.acquire_index_lock(),
            Err(NativeGitIndexError::IndexLocked)
        ));
        assert_eq!(
            runner.index_bytes().expect("index under contention"),
            before
        );
    }

    #[test]
    fn intent_to_add_index_is_snapshot_exact_and_preview_only() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("new.txt"), "new\n").expect("new worktree file");
        git(directory.path(), &["add", "--intent-to-add", "new.txt"]);
        assert!(
            runner.has_intent_to_add().expect("intent-to-add probe"),
            "ls-files debug output: {}",
            git_value(directory.path(), &["ls-files", "--debug"])
        );
        let snapshot = exact_snapshot(&assembler, &runner);
        assert_eq!(snapshot.index.state, RepositoryIndexStateV1::IntentToAdd);
        assert_eq!(
            unsupported_state(&snapshot, &runner),
            Some(GitIndexUnsupportedStateV1::IntentToAdd)
        );
        assert!(!snapshot.is_mutation_eligible());
    }

    #[test]
    fn commit_rejects_empty_index_and_stale_ref_before_commit_object_creation() {
        let (directory, assembler, runner) = repository_fixture();
        let intent = commit_intent("transaction commit\n");
        let empty = commit_preview(
            &assembler,
            &runner,
            "git-index-preview.empty-commit",
            &intent,
        );
        let lock = runner.acquire_index_lock().expect("empty commit lock");
        assert!(matches!(
            runner.commit_index(&lock, &empty, &intent),
            Err(NativeGitIndexError::EmptyIndexCommit)
        ));
        drop(lock);

        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        git(directory.path(), &["add", "packet.txt"]);
        let stale = commit_preview(&assembler, &runner, "git-index-preview.stale-ref", &intent);
        git(directory.path(), &["commit", "--quiet", "-m", "external"]);
        let object_path = directory.path().join(".git").join("objects");
        let objects_before = object_file_count(&object_path);
        let head_before = git_value(directory.path(), &["rev-parse", "HEAD"]);
        let lock = runner.acquire_index_lock().expect("stale ref lock");
        assert!(matches!(
            runner.commit_index(&lock, &stale, &intent),
            Err(NativeGitIndexError::StaleRepositoryState)
        ));
        drop(lock);
        assert_eq!(
            git_value(directory.path(), &["rev-parse", "HEAD"]),
            head_before
        );
        assert_eq!(object_file_count(&object_path), objects_before);
    }

    #[test]
    fn commit_intent_mismatch_fails_before_object_or_ref_mutation() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        git(directory.path(), &["add", "packet.txt"]);
        let previewed_intent = commit_intent("previewed message\n");
        let preview = commit_preview(
            &assembler,
            &runner,
            "git-index-preview.intent-mismatch",
            &previewed_intent,
        );
        let replacement_intent = commit_intent("replacement message\n");
        let object_path = directory.path().join(".git").join("objects");
        let objects_before = object_file_count(&object_path);
        let head_before = git_value(directory.path(), &["rev-parse", "HEAD"]);

        let lock = runner.acquire_index_lock().expect("commit lock");
        assert!(matches!(
            runner.commit_index(&lock, &preview, &replacement_intent),
            Err(NativeGitIndexError::CommitIntentMismatch)
        ));
        drop(lock);

        assert_eq!(object_file_count(&object_path), objects_before);
        assert_eq!(
            git_value(directory.path(), &["rev-parse", "HEAD"]),
            head_before
        );
    }

    #[test]
    fn signing_failure_is_safe_and_wrong_ref_never_advances() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        git(directory.path(), &["add", "packet.txt"]);
        let mut signed_intent = commit_intent("signed transaction\n");
        signed_intent.signing_policy = GitIndexSigningPolicyV1::SignatureRequired {
            key_reference: "tracedecay-missing-signing-key".to_owned(),
        };
        signed_intent.validate().expect("signed intent");
        let signed_preview = commit_preview(
            &assembler,
            &runner,
            "git-index-preview.signing-failure",
            &signed_intent,
        );
        let head_before = git_value(directory.path(), &["rev-parse", "HEAD"]);
        let lock = runner.acquire_index_lock().expect("signed commit lock");
        let signing_error = runner
            .commit_index(&lock, &signed_preview, &signed_intent)
            .expect_err("missing signing key must fail");
        drop(lock);
        assert!(!signing_error.is_commit_boundary_unknown());
        assert_eq!(
            git_value(directory.path(), &["rev-parse", "HEAD"]),
            head_before
        );

        let unsigned_intent = commit_intent("wrong ref transaction\n");
        let wrong_ref_preview = commit_preview(
            &assembler,
            &runner,
            "git-index-preview.wrong-ref",
            &unsigned_intent,
        );
        git(directory.path(), &["checkout", "-q", "-b", "other"]);
        let other_before = git_value(directory.path(), &["rev-parse", "HEAD"]);
        let lock = runner.acquire_index_lock().expect("wrong ref lock");
        assert!(matches!(
            runner.commit_index(&lock, &wrong_ref_preview, &unsigned_intent),
            Err(NativeGitIndexError::StaleRepositoryState
                | NativeGitIndexError::CommitStateUnsupported)
        ));
        drop(lock);
        assert_eq!(
            git_value(directory.path(), &["rev-parse", "HEAD"]),
            other_before
        );
    }

    #[test]
    fn commit_advances_only_the_previewed_ref_to_the_previewed_tree() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        git(directory.path(), &["add", "packet.txt"]);
        let intent = commit_intent("transaction commit\n");
        let preview = commit_preview(&assembler, &runner, "git-index-preview.commit", &intent);
        let old_head = git_value(directory.path(), &["rev-parse", "HEAD"]);
        let lock = runner.acquire_index_lock().expect("commit lock");
        let commit = runner
            .commit_index(&lock, &preview, &intent)
            .expect("commit exact index");
        drop(lock);
        assert_eq!(
            git_value(directory.path(), &["rev-parse", "HEAD"]),
            commit.as_str()
        );
        assert_eq!(
            git_value(directory.path(), &["rev-parse", "HEAD^"]),
            old_head
        );
        assert_eq!(
            git_value(directory.path(), &["rev-parse", "HEAD^{tree}"]),
            preview
                .candidate_index_tree
                .as_ref()
                .expect("candidate tree")
                .as_str()
        );
        let lock = runner.acquire_index_lock().expect("commit snapshot lock");
        let committed = assembler
            .capture_snapshot(&preview.repository_snapshot, &runner, &lock)
            .expect("committed snapshot");
        drop(lock);
        assert!(live_result_matches_preview(
            directory.path(),
            &preview,
            &committed,
            Some(&commit)
        ));

        git(directory.path(), &["checkout", "-q", "-b", "same-tip"]);
        let lock = runner
            .acquire_index_lock()
            .expect("branch drift snapshot lock");
        let branch_drift = assembler
            .capture_snapshot(&preview.repository_snapshot, &runner, &lock)
            .expect("branch drift snapshot");
        drop(lock);
        assert!(
            !live_result_matches_preview(directory.path(), &preview, &branch_drift, Some(&commit)),
            "the same commit on a different attached branch is not the previewed HEAD state"
        );
    }

    /// Portable stand-in for macOS `/tmp` → `/private/tmp`: capture through a
    /// symlink alias must mint the same snapshot the daemon recaptures from
    /// the canonical root. Exact CAS stays strict — content drift still fails.
    #[cfg(unix)]
    #[test]
    fn snapshot_capture_agrees_across_symlink_repository_root_aliases() {
        use std::os::unix::fs::symlink;

        let (directory, _assembler, runner) = repository_fixture();
        let alias_parent = tempfile::tempdir().expect("alias parent");
        let alias = alias_parent.path().join("repo-alias");
        symlink(directory.path(), &alias).expect("repository root symlink alias");

        let project_id = ProjectId::new("project.fixture").expect("project id");
        let repository_id = RepositoryId::new("repository.fixture").expect("repository id");
        let worktree_id = WorktreeId::new("worktree.fixture").expect("worktree id");
        let captured_at = UtcMicros(1);

        let via_real = capture_exact_snapshot_for_test(
            directory.path(),
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            captured_at,
        )
        .expect("real-path snapshot");
        let via_alias = capture_exact_snapshot_for_test(
            &alias,
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            captured_at,
        )
        .expect("alias-path snapshot");
        assert_eq!(
            via_real, via_alias,
            "alias and real roots must produce identical snapshot identity at capture"
        );
        assert_ne!(
            alias.as_os_str(),
            directory
                .path()
                .canonicalize()
                .expect("canonical root")
                .as_os_str(),
            "fixture must exercise a non-canonical alias path"
        );

        // Daemon-mounted assembler stores the canonical root; caller snapshot
        // arrived via the alias. Recapture must compare equal without loosening
        // PartialEq.
        let daemon_assembler = NativeGitIndexPreviewAssembler::new(
            directory.path().canonicalize().expect("canonical root"),
            project_id,
            repository_id,
            worktree_id,
        );
        let lock = runner.acquire_index_lock().expect("alias CAS lock");
        let recaptured = daemon_assembler
            .capture_snapshot(&via_alias, &runner, &lock)
            .expect("daemon recapture through canonical root");
        drop(lock);
        assert_eq!(
            recaptured, via_alias,
            "daemon recapture must match caller snapshot captured through a symlink alias"
        );

        fs::write(directory.path().join("packet.txt"), "drifted\n").expect("content drift");
        let lock = runner.acquire_index_lock().expect("drift lock");
        let drifted = daemon_assembler
            .capture_snapshot(&via_alias, &runner, &lock)
            .expect("drift recapture");
        drop(lock);
        assert_ne!(
            drifted, via_alias,
            "exact CAS must still reject genuine content drift after alias canonicalization"
        );
        let stale = commit_request(
            via_alias,
            commit_intent("alias stale preview\n"),
            "git-index-preview.alias-stale",
        );
        assert!(
            matches!(
                daemon_assembler.materialize(&stale),
                Err(GitIndexTransactionPortError::StalePreview)
            ),
            "preview CAS must report stale_preview for drifted state, never succeed"
        );
    }
}
