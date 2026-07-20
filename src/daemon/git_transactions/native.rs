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
    GitDegradationV1, GitDiffScopeV1, GitHeadStateV1, GitIndexCommitIntentV1,
    GitIndexPreviewDispositionV1, GitIndexPreviewId, GitIndexPreviewV1, GitIndexReceiptId,
    GitIndexReceiptOutcomeV1, GitIndexTransactionId, GitIndexTransactionOperationV1,
    GitIndexTransactionReceiptV1, GitIndexUnsupportedStateV1, GitStatusEntryV1, ManifestDigest,
    ProjectId, RepositoryId, RepositoryIndexSnapshotV1, RepositoryIndexStateV1,
    RepositoryStateSnapshotV1, RepositoryWorkingTreeSnapshotV1, RepositoryWorkingTreeStateV1,
    UtcMicros, WorktreeId, canonical_sha256,
};
use tracedecay_store::GitIndexTransactionRecordV1;

use crate::git_index_transactions::{
    FixedGitIndexRunner, GIT_INDEX_ADAPTER_REVISION, NativeGitIndexError, NativeIndexLock,
    ValidatedIndexPatch,
};
use crate::git_intelligence::NativeGitIntelligence;

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

/// Concrete PR11 assembler backed by the fixed PR9 read-only authority and
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
        Self {
            repository_root: repository_root.into(),
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
        let status = self
            .read_authority()
            .status()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
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
        .map_err(|_| GitIndexTransactionPortError::StalePreview)
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

impl GitIndexPreviewAssembler for NativeGitIndexPreviewAssembler {
    fn materialize(
        &self,
        request: &GitIndexPreviewRequestV1,
    ) -> Result<MaterializedGitIndexPreview, GitIndexTransactionPortError> {
        request
            .validate()
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let runner = self.runner()?;
        let lock = runner.acquire_index_lock().map_err(map_native_error)?;
        let current = self.capture_snapshot(&request.repository_snapshot, &runner, &lock)?;
        if current != request.repository_snapshot {
            return Err(GitIndexTransactionPortError::StalePreview);
        }
        let snapshot_digest = GitIndexPreviewV1::repository_snapshot_digest(&current)
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        let disposition = unsupported_state(&current, &runner).map_or(
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
        let preview = GitIndexPreviewV1::new(
            request.preview_id.clone(),
            request.binding.operation,
            current,
            snapshot_digest,
            selected_hunks,
            candidate_index_tree,
            disposition,
            request.observed_at,
            expires_at,
        )
        .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
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
        if preview.preview.candidate_index_tree != current.index.tree_id {
            return Err(GitIndexTransactionPortError::NeedsInspection);
        }
        if preview.preview.operation == GitIndexTransactionOperationV1::CommitIndex
            && current.head.commit() != created_commit
        {
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
        let (outcome, created_commit) = if &current == old {
            (GitIndexReceiptOutcomeV1::AbortedNoChange, None)
        } else if record.preview.operation != GitIndexTransactionOperationV1::CommitIndex
            && current.index.tree_id == record.preview.candidate_index_tree
        {
            (GitIndexReceiptOutcomeV1::Committed, None)
        } else if record.preview.operation == GitIndexTransactionOperationV1::CommitIndex
            && current.index.tree_id == record.preview.candidate_index_tree
            && commit_matches_preview(&self.repository_root, &record.preview, &current)
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
                    } else if !snapshot.coverage.is_complete() {
                        Some(GitIndexUnsupportedStateV1::UnreadableWorkingTree)
                    } else {
                        None
                    }
                }
            }
        }
    }
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
    preview: &GitIndexPreviewV1,
    current: &RepositoryStateSnapshotV1,
) -> bool {
    let (Some(head), Some(expected_tree), Some(old_head)) = (
        current.head.commit(),
        preview.candidate_index_tree.as_ref(),
        preview.repository_snapshot.head.commit(),
    ) else {
        return false;
    };
    let tree_expression = format!("{}^{{tree}}", head.as_str());
    let parent_expression = format!("{}^", head.as_str());
    let tree = read_git_value(repository_root, &tree_expression);
    let parent = read_git_value(repository_root, &parent_expression);
    tree.as_deref() == Some(expected_tree.as_str()) && parent.as_deref() == Some(old_head.as_str())
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
        match previews.get(&materialized.preview.preview_id) {
            Some(existing)
                if existing.preview.preview_digest != materialized.preview.preview_digest =>
            {
                return Err(GitIndexTransactionPortError::StalePreview);
            }
            Some(_) => {}
            None => {
                previews.insert(materialized.preview.preview_id.clone(), materialized);
            }
        }
        Ok(result)
    }

    fn apply(
        &self,
        transaction_id: &GitIndexTransactionId,
        preview: &GitIndexPreviewV1,
        request: &GitIndexApplyRequestV1,
    ) -> Result<NativeGitIndexApplyResult, GitIndexTransactionPortError> {
        let materialized = self
            .previews
            .lock()
            .map_err(|_| GitIndexTransactionPortError::DaemonUnavailable)?
            .get(&preview.preview_id)
            .cloned()
            .ok_or(GitIndexTransactionPortError::StalePreview)?;
        if materialized.preview != *preview {
            return Err(GitIndexTransactionPortError::StalePreview);
        }
        let mut index_lock = materialized
            .runner
            .acquire_index_lock()
            .map_err(map_native_error)?;
        let current = self.assembler.capture_current(&materialized, &index_lock)?;
        let current_digest = GitIndexPreviewV1::repository_snapshot_digest(&current)
            .map_err(|_| GitIndexTransactionPortError::StalePreview)?;
        if current != preview.repository_snapshot
            || current_digest != preview.repository_snapshot_digest
        {
            return Err(GitIndexTransactionPortError::StalePreview);
        }
        let current_patches = self.assembler.revalidate_patches(&materialized)?;
        if current_patches.len() != materialized.patches.len() {
            return Err(GitIndexTransactionPortError::StalePreview);
        }

        let created_commit = match preview.operation {
            GitIndexTransactionOperationV1::StageHunks => {
                materialized
                    .runner
                    .stage_hunks(&mut index_lock, preview, &current_patches)
                    .map_err(map_native_error)?;
                None
            }
            GitIndexTransactionOperationV1::UnstageHunks => {
                materialized
                    .runner
                    .unstage_hunks(&mut index_lock, preview, &current_patches)
                    .map_err(map_native_error)?;
                None
            }
            GitIndexTransactionOperationV1::CommitIndex => Some(
                materialized
                    .runner
                    .commit_index(
                        &index_lock,
                        preview,
                        materialized
                            .commit_intent
                            .as_ref()
                            .ok_or(GitIndexTransactionPortError::StalePreview)?,
                    )
                    .map_err(map_native_error)?,
            ),
        };
        drop(index_lock);
        self.assembler.finalize(
            &materialized,
            transaction_id,
            request,
            created_commit.as_ref(),
        )
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
        | NativeGitIndexError::StaleRepositoryState
        | NativeGitIndexError::MalformedOutput { .. }
        | NativeGitIndexError::Domain(_) => GitIndexTransactionPortError::StalePreview,
        NativeGitIndexError::RepositoryUnavailable(_)
        | NativeGitIndexError::GitFailed { .. }
        | NativeGitIndexError::Io(_) => GitIndexTransactionPortError::NeedsInspection,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;
    use tracedecay_domain::{
        GitCommitIdentityV1, GitCoverageV1, GitIndexSigningPolicyV1, GitObjectFormatV1,
        GitOperationStateV1,
    };

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
    ) -> GitIndexPreviewV1 {
        let snapshot = exact_snapshot(assembler, runner);
        let snapshot_digest =
            GitIndexPreviewV1::repository_snapshot_digest(&snapshot).expect("snapshot digest");
        GitIndexPreviewV1::new(
            GitIndexPreviewId::new(preview_id).expect("preview id"),
            GitIndexTransactionOperationV1::CommitIndex,
            snapshot.clone(),
            snapshot_digest,
            Vec::new(),
            snapshot.index.tree_id,
            GitIndexPreviewDispositionV1::Applicable,
            UtcMicros(2),
            UtcMicros(3),
        )
        .expect("commit preview")
    }

    fn commit_intent() -> GitIndexCommitIntentV1 {
        let identity = GitCommitIdentityV1 {
            name: "TraceDecay Test".to_owned(),
            email: "tracedecay@example.com".to_owned(),
            at: UtcMicros(1_000_000),
        };
        GitIndexCommitIntentV1::new(
            "transaction commit\n".to_owned(),
            identity.clone(),
            identity,
            GitIndexSigningPolicyV1::UnsignedPermitted,
        )
        .expect("commit intent")
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
        let patch = extract_patch(directory.path(), &GitDiffScopeV1::WorkingTree, &hunk)
            .expect("extract exact packet");
        let patch = ValidatedIndexPatch::new(hunk, patch).expect("validate exact packet");
        let runner = FixedGitIndexRunner::new(directory.path()).expect("runner");
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
        let empty = commit_preview(&assembler, &runner, "git-index-preview.empty-commit");
        let lock = runner.acquire_index_lock().expect("empty commit lock");
        assert!(matches!(
            runner.commit_index(&lock, &empty, &commit_intent()),
            Err(NativeGitIndexError::EmptyIndexCommit)
        ));
        drop(lock);

        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        git(directory.path(), &["add", "packet.txt"]);
        let stale = commit_preview(&assembler, &runner, "git-index-preview.stale-ref");
        git(directory.path(), &["commit", "--quiet", "-m", "external"]);
        let object_path = directory.path().join(".git").join("objects");
        let objects_before = object_file_count(&object_path);
        let head_before = git_value(directory.path(), &["rev-parse", "HEAD"]);
        let lock = runner.acquire_index_lock().expect("stale ref lock");
        assert!(matches!(
            runner.commit_index(&lock, &stale, &commit_intent()),
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
    fn commit_advances_only_the_previewed_ref_to_the_previewed_tree() {
        let (directory, assembler, runner) = repository_fixture();
        fs::write(directory.path().join("packet.txt"), "after\n").expect("change worktree");
        git(directory.path(), &["add", "packet.txt"]);
        let preview = commit_preview(&assembler, &runner, "git-index-preview.commit");
        let old_head = git_value(directory.path(), &["rev-parse", "HEAD"]);
        let lock = runner.acquire_index_lock().expect("commit lock");
        let commit = runner
            .commit_index(&lock, &preview, &commit_intent())
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
    }
}
