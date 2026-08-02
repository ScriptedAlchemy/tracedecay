//! Fixed native Git mechanics for PR11 index transactions.
//!
//! The native surface only stages, unstages, and commits preview-bound index
//! changes. It never accepts a generic Git subcommand, flags, ref, or
//! working-tree path from a caller. Daemon code supplies already validated,
//! preview-bound patch material and performs the journaled recovery protocol.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use thiserror::Error;
use tracedecay_domain::{
    DomainError, GitBlobExpectationV1, GitFileModeV1, GitHeadStateV1, GitIndexCommitIntentV1,
    GitIndexEntryExpectationV1, GitIndexPreviewDispositionV1, GitIndexPreviewV1,
    GitIndexSigningPolicyV1, GitIndexTransactionOperationV1, GitOidV1, HunkDirectionV1, HunkRefV1,
    ManifestDigest, canonical_sha256,
};

pub(crate) const GIT_INDEX_ADAPTER_REVISION: &str = "tracedecay.git-index-adapter.v1";

mod patch;
mod process;
#[cfg(test)]
mod tests;

pub(crate) use patch::ValidatedIndexPatch;
use process::{
    absolute_git_path, current_operation_state, git_command, git_timestamp, is_executable_hook,
    joined_patch_bytes, parse_git_oid, read_optional_file, run_command_with_stdin, run_git_at,
    sync_parent_directory, worktree_mode,
};

#[derive(Debug, Error)]
pub enum NativeGitIndexError {
    #[error("repository root is unavailable: {0}")]
    RepositoryUnavailable(String),
    #[error("Git index is locked")]
    IndexLocked,
    #[error("native Git output was malformed for {operation}")]
    MalformedOutput { operation: &'static str },
    #[error("native Git {operation} failed with status {status}")]
    GitFailed {
        operation: &'static str,
        status: String,
    },
    #[error("a patch does not exactly match its preview-bound HunkRef")]
    PatchDoesNotMatchHunk,
    #[error("partial hunk selection has no proven round-trip adapter")]
    PartialHunkSelectionUnsupported,
    #[error("native commit is unavailable for the previewed repository state")]
    CommitStateUnsupported,
    #[error("configured Git hooks make commit_index preview-only")]
    UnsupportedHookPolicy,
    #[error("native write-tree differs from the preview candidate tree")]
    CandidateTreeMismatch,
    #[error("the full commit intent does not match the preview commitment")]
    CommitIntentMismatch,
    #[error("native repository state no longer matches the preview")]
    StaleRepositoryState,
    #[error("the previewed index tree is identical to the current HEAD tree")]
    EmptyIndexCommit,
    #[error("native Git I/O failed: {0}")]
    Io(String),
    #[error("native Git {operation} crossed a commit boundary with an unknown result: {detail}")]
    CommitBoundaryUnknown {
        operation: &'static str,
        detail: String,
    },
    #[error(transparent)]
    Domain(#[from] DomainError),
}

impl NativeGitIndexError {
    /// Preserve ambiguity once a native publication/commit boundary may have
    /// happened. Callers must reconcile rather than retry the operation.
    pub(crate) fn into_commit_boundary_unknown(self, operation: &'static str) -> Self {
        match self {
            Self::CommitBoundaryUnknown { .. } => self,
            error => Self::CommitBoundaryUnknown {
                operation,
                detail: error.to_string(),
            },
        }
    }

    pub(crate) const fn is_commit_boundary_unknown(&self) -> bool {
        matches!(self, Self::CommitBoundaryUnknown { .. })
    }
}

/// The fixed native executor rooted at a daemon-resolved repository. Callers
/// cannot change its working directory or process environment.
#[derive(Clone, Debug)]
pub(crate) struct FixedGitIndexRunner {
    repository_root: PathBuf,
    git_dir: PathBuf,
    index_path: PathBuf,
    objects_path: PathBuf,
}

/// Exclusive ownership of Git's real `index.lock` path. The lock is acquired
/// with `create_new` and removed on every non-commit exit. Index-changing
/// operations build a private candidate index and publish it by renaming this
/// exact lock file over the real index.
pub(crate) struct NativeIndexLock {
    path: PathBuf,
    file: File,
    published: bool,
}

impl Drop for NativeIndexLock {
    fn drop(&mut self) {
        if !self.published {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl FixedGitIndexRunner {
    pub(crate) fn new(repository_root: impl AsRef<Path>) -> Result<Self, NativeGitIndexError> {
        let repository_root = repository_root
            .as_ref()
            .canonicalize()
            .map_err(|error| NativeGitIndexError::RepositoryUnavailable(error.to_string()))?;
        let git_dir_text = run_git_at(&repository_root, "rev-parse", &["rev-parse", "--git-dir"])?;
        let git_dir = absolute_git_path(&repository_root, &git_dir_text)?;
        let index_path_text = run_git_at(
            &repository_root,
            "rev-parse",
            &["rev-parse", "--git-path", "index"],
        )?;
        let index_path = absolute_git_path(&repository_root, &index_path_text)?;
        let objects_path_text = run_git_at(
            &repository_root,
            "rev-parse",
            &["rev-parse", "--git-path", "objects"],
        )?;
        let objects_path = absolute_git_path(&repository_root, &objects_path_text)?;
        Ok(Self {
            repository_root,
            git_dir,
            index_path,
            objects_path,
        })
    }

    pub(crate) fn ensure_index_unlocked(&self) -> Result<(), NativeGitIndexError> {
        if self.index_lock_path().exists() {
            return Err(NativeGitIndexError::IndexLocked);
        }
        Ok(())
    }

    pub(crate) fn repository_root(&self) -> &Path {
        &self.repository_root
    }

    pub(crate) fn is_bare_repository(&self) -> Result<bool, NativeGitIndexError> {
        let output = self.run_git("rev-parse", &["rev-parse", "--is-bare-repository"])?;
        match String::from_utf8(output.stdout)
            .ok()
            .map(|value| value.trim().to_owned())
            .as_deref()
        {
            Some("true") => Ok(true),
            Some("false") => Ok(false),
            _ => Err(NativeGitIndexError::MalformedOutput {
                operation: "rev-parse",
            }),
        }
    }

    pub(crate) fn object_format(&self) -> Result<String, NativeGitIndexError> {
        let output = self.run_git("rev-parse", &["rev-parse", "--show-object-format"])?;
        String::from_utf8(output.stdout)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or(NativeGitIndexError::MalformedOutput {
                operation: "rev-parse",
            })
    }

    pub(crate) fn has_applicable_commit_hooks(&self) -> Result<bool, NativeGitIndexError> {
        match self.ensure_no_applicable_hooks() {
            Ok(()) => Ok(false),
            Err(NativeGitIndexError::UnsupportedHookPolicy) => Ok(true),
            Err(error) => Err(error),
        }
    }

    pub(crate) fn signing_key_available(
        &self,
        key_reference: &str,
    ) -> Result<bool, NativeGitIndexError> {
        let format = self.run_git_output(&["config", "--get", "gpg.format"])?;
        let format = if format.status.success() {
            String::from_utf8(format.stdout)
                .map_err(|_| NativeGitIndexError::MalformedOutput {
                    operation: "config",
                })?
                .trim()
                .to_owned()
        } else if format.status.code() == Some(1) {
            "openpgp".to_owned()
        } else {
            return Err(NativeGitIndexError::GitFailed {
                operation: "config",
                status: format.status.to_string(),
            });
        };
        if format != "openpgp" {
            // SSH and X.509 key availability can depend on an agent or
            // provider that cannot be proven by a read-only native probe.
            return Ok(false);
        }
        let configured_program = self.run_git_output(&["config", "--get", "gpg.program"])?;
        if configured_program.status.success() && !configured_program.stdout.is_empty() {
            // Apply would use an arbitrary configured provider. V1 does not
            // execute it during preview, so availability remains unproven.
            return Ok(false);
        }
        if !configured_program.status.success() && configured_program.status.code() != Some(1) {
            return Err(NativeGitIndexError::GitFailed {
                operation: "config",
                status: configured_program.status.to_string(),
            });
        }
        let output = Command::new("gpg")
            .args([
                "--batch",
                "--list-secret-keys",
                "--with-colons",
                "--",
                key_reference,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
        Ok(output.is_ok_and(|output| output.status.success()))
    }

    pub(crate) fn acquire_index_lock(&self) -> Result<NativeIndexLock, NativeGitIndexError> {
        let path = self.index_lock_path();
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::AlreadyExists {
                    NativeGitIndexError::IndexLocked
                } else {
                    NativeGitIndexError::Io(error.to_string())
                }
            })?;
        Ok(NativeIndexLock {
            path,
            file,
            published: false,
        })
    }

    pub(crate) fn git_version(&self) -> Result<String, NativeGitIndexError> {
        run_git_at(&self.repository_root, "version", &["--version"])
    }

    pub(crate) fn refs_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let output = self.run_git(
            "for-each-ref",
            &[
                "for-each-ref",
                "--format=%(refname)%00%(objectname)%00%(symref)",
            ],
        )?;
        canonical_sha256(&output.stdout).map_err(Into::into)
    }

    pub(crate) fn head_state(&self) -> Result<GitHeadStateV1, NativeGitIndexError> {
        let symbolic = self.run_git_output(&["symbolic-ref", "-q", "HEAD"])?;
        let branch = symbolic
            .status
            .success()
            .then(|| String::from_utf8(symbolic.stdout).ok())
            .flatten()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        let head = self.run_git_output(&["rev-parse", "--verify", "HEAD"])?;
        if head.status.success() {
            let commit = parse_git_oid("rev-parse", &head.stdout)?;
            Ok(match branch {
                Some(branch) => GitHeadStateV1::Attached { branch, commit },
                None => GitHeadStateV1::Detached { commit },
            })
        } else if let Some(branch) = branch {
            Ok(GitHeadStateV1::Unborn { branch })
        } else {
            Err(NativeGitIndexError::MalformedOutput {
                operation: "rev-parse",
            })
        }
    }

    pub(crate) fn tracked_worktree_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let output = self.run_git(
            "diff",
            &[
                "diff",
                "--no-ext-diff",
                "--no-color",
                "--binary",
                "--full-index",
            ],
        )?;
        canonical_sha256(&output.stdout).map_err(Into::into)
    }

    pub(crate) fn configuration_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let output = self.run_git("config", &["config", "--null", "--show-origin", "--list"])?;
        canonical_sha256(&output.stdout).map_err(Into::into)
    }

    pub(crate) fn sparse_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let sparse_path = absolute_git_path(
            &self.repository_root,
            &run_git_at(
                &self.repository_root,
                "rev-parse",
                &["rev-parse", "--git-path", "info/sparse-checkout"],
            )?,
        )?;
        let sparse_bytes = read_optional_file(&sparse_path)?;
        canonical_sha256(&sparse_bytes).map_err(Into::into)
    }

    pub(crate) fn submodule_digest(&self) -> Result<ManifestDigest, NativeGitIndexError> {
        let output = self.run_git("ls-files", &["ls-files", "--stage", "-z"])?;
        let gitmodules = read_optional_file(&self.repository_root.join(".gitmodules"))?;
        canonical_sha256(&(output.stdout, gitmodules)).map_err(Into::into)
    }

    pub(crate) fn has_intent_to_add(&self) -> Result<bool, NativeGitIndexError> {
        const CE_INTENT_TO_ADD: u32 = 0x2000_0000;
        let output = self.run_git("ls-files", &["ls-files", "--debug"])?;
        let text =
            String::from_utf8(output.stdout).map_err(|_| NativeGitIndexError::MalformedOutput {
                operation: "ls-files",
            })?;
        Ok(text.lines().any(|line| {
            line.split_once("flags: ")
                .map(|(_, flags)| flags)
                .and_then(|flags| u32::from_str_radix(flags, 16).ok())
                .is_some_and(|flags| flags & CE_INTENT_TO_ADD != 0)
        }))
    }

    pub(crate) fn index_tree_under_lock(
        &self,
        lock: &NativeIndexLock,
    ) -> Result<GitOidV1, NativeGitIndexError> {
        self.require_lock(lock)?;
        self.preview_candidate_tree_inner(&[], false)
    }

    pub(crate) fn stage_hunks(
        &self,
        lock: &mut NativeIndexLock,
        preview: &GitIndexPreviewV1,
        patches: &[ValidatedIndexPatch],
    ) -> Result<(), NativeGitIndexError> {
        self.apply_hunks(
            lock,
            preview,
            patches,
            HunkDirectionV1::WorkingTreeToIndex,
            false,
        )
    }

    pub(crate) fn unstage_hunks(
        &self,
        lock: &mut NativeIndexLock,
        preview: &GitIndexPreviewV1,
        patches: &[ValidatedIndexPatch],
    ) -> Result<(), NativeGitIndexError> {
        self.apply_hunks(lock, preview, patches, HunkDirectionV1::IndexToHead, true)
    }

    #[cfg(test)]
    pub(crate) fn write_tree(&self) -> Result<GitOidV1, NativeGitIndexError> {
        self.ensure_index_unlocked()?;
        let output = self.run_git("write-tree", &["write-tree"])?;
        parse_git_oid("write-tree", &output.stdout)
    }

    /// Compute the candidate tree against an isolated index and object
    /// quarantine. Preview never writes the repository index or object store.
    #[cfg(test)]
    pub(crate) fn preview_candidate_tree(
        &self,
        patches: &[ValidatedIndexPatch],
        reverse: bool,
    ) -> Result<GitOidV1, NativeGitIndexError> {
        let lock = self.acquire_index_lock()?;
        let result = self.preview_candidate_tree_under_lock(&lock, patches, reverse);
        drop(lock);
        result
    }

    pub(crate) fn preview_candidate_tree_under_lock(
        &self,
        lock: &NativeIndexLock,
        patches: &[ValidatedIndexPatch],
        reverse: bool,
    ) -> Result<GitOidV1, NativeGitIndexError> {
        self.require_lock(lock)?;
        self.preview_candidate_tree_inner(patches, reverse)
    }

    fn preview_candidate_tree_inner(
        &self,
        patches: &[ValidatedIndexPatch],
        reverse: bool,
    ) -> Result<GitOidV1, NativeGitIndexError> {
        let quarantine =
            tempfile::tempdir().map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        let index_path = quarantine.path().join("index");
        if self.index_path.exists() {
            std::fs::copy(&self.index_path, &index_path)
                .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        } else {
            std::fs::File::create(&index_path)
                .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        }
        let object_path = quarantine.path().join("objects");
        std::fs::create_dir_all(&object_path)
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;

        let patch_bytes = joined_patch_bytes(patches);
        if !patch_bytes.is_empty() {
            let mut apply = self.quarantine_command(&index_path, &object_path);
            apply
                .arg("apply")
                .arg("--cached")
                .arg("--recount")
                .arg("--whitespace=nowarn")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if reverse {
                apply.arg("--reverse");
            }
            run_command_with_stdin(apply, "preview apply", &patch_bytes)?;
        }

        let output = self
            .quarantine_command(&index_path, &object_path)
            .arg("write-tree")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        if !output.status.success() {
            return Err(NativeGitIndexError::GitFailed {
                operation: "preview write-tree",
                status: output.status.to_string(),
            });
        }
        parse_git_oid("preview write-tree", &output.stdout)
    }

    pub(crate) fn index_bytes(&self) -> Result<Vec<u8>, NativeGitIndexError> {
        if self.index_path.exists() {
            std::fs::read(&self.index_path)
                .map_err(|error| NativeGitIndexError::Io(error.to_string()))
        } else {
            Ok(Vec::new())
        }
    }

    /// Create exactly one ordinary commit from the previewed index tree and
    /// advance only the previewed attached ref with an old-value CAS.
    pub(crate) fn commit_index(
        &self,
        lock: &NativeIndexLock,
        preview: &GitIndexPreviewV1,
        intent: &GitIndexCommitIntentV1,
    ) -> Result<GitOidV1, NativeGitIndexError> {
        preview.validate()?;
        intent.validate()?;
        if preview.commit_intent_digest.as_ref() != Some(&intent.compute_digest()?) {
            return Err(NativeGitIndexError::CommitIntentMismatch);
        }
        if preview.operation != GitIndexTransactionOperationV1::CommitIndex
            || !matches!(
                preview.disposition,
                GitIndexPreviewDispositionV1::Applicable
            )
        {
            return Err(NativeGitIndexError::CommitStateUnsupported);
        }
        let GitHeadStateV1::Attached { branch, commit } = &preview.repository_snapshot.head else {
            return Err(NativeGitIndexError::CommitStateUnsupported);
        };
        self.require_lock(lock)?;
        self.verify_native_preconditions(lock, preview)?;
        self.ensure_no_applicable_hooks()?;
        let current_ref = self.run_git("symbolic-ref", &["symbolic-ref", "-q", "HEAD"])?;
        let current_ref = String::from_utf8(current_ref.stdout)
            .map_err(|_| NativeGitIndexError::MalformedOutput {
                operation: "symbolic-ref",
            })?
            .trim()
            .to_owned();
        if current_ref.is_empty()
            || (current_ref != branch.as_str()
                && current_ref
                    .strip_prefix("refs/heads/")
                    .is_none_or(|short| short != branch.as_str()))
        {
            return Err(NativeGitIndexError::CommitStateUnsupported);
        }
        self.require_ref_value(&current_ref, commit)?;

        let tree = self.index_tree_under_lock(lock)?;
        if preview.candidate_index_tree.as_ref() != Some(&tree) {
            return Err(NativeGitIndexError::CandidateTreeMismatch);
        }
        let parent_tree_expression = format!("{}^{{tree}}", commit.as_str());
        let parent_tree = self.run_git(
            "rev-parse",
            &["rev-parse", "--verify", &parent_tree_expression],
        )?;
        if parse_git_oid("rev-parse", &parent_tree.stdout)? == tree {
            return Err(NativeGitIndexError::EmptyIndexCommit);
        }

        // `commit-tree` cannot participate in the ref transaction. Reject a
        // ref that was already stale before creating any durable commit
        // object, then rely on update-ref's old-value CAS for the remaining
        // unavoidable race.
        self.require_ref_value(&current_ref, commit)?;
        let durable_tree = self.write_tree_durable_under_lock(lock)?;
        if durable_tree != tree {
            return Err(NativeGitIndexError::CandidateTreeMismatch);
        }
        self.require_ref_value(&current_ref, commit)?;

        let mut command = self.command();
        command
            .arg("commit-tree")
            .arg(tree.as_str())
            .arg("-p")
            .arg(commit.as_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("GIT_AUTHOR_NAME", &intent.author.name)
            .env("GIT_AUTHOR_EMAIL", &intent.author.email)
            .env("GIT_AUTHOR_DATE", git_timestamp(intent.author.at.0))
            .env("GIT_COMMITTER_NAME", &intent.committer.name)
            .env("GIT_COMMITTER_EMAIL", &intent.committer.email)
            .env("GIT_COMMITTER_DATE", git_timestamp(intent.committer.at.0));
        if let GitIndexSigningPolicyV1::SignatureRequired { key_reference } = &intent.signing_policy
        {
            command.arg(format!("-S{key_reference}"));
        }
        // `commit-tree` may write an unreachable object, but it cannot publish
        // an index or ref mutation. Hook/signing/process failures here are
        // therefore proven no-change at the PR11 repository-state boundary.
        let output = run_command_with_stdin(command, "commit-tree", intent.message.as_bytes())?;
        let created_commit = parse_git_oid("commit-tree", &output.stdout)?;

        let update = self
            .run_git_output(&[
                "update-ref",
                &current_ref,
                created_commit.as_str(),
                commit.as_str(),
            ])
            .map_err(|error| error.into_commit_boundary_unknown("update-ref"))?;
        if !update.status.success() {
            // The old-value CAS prevents competing ref updates, but a killed
            // or failing process can still report non-success after Git has
            // crossed its ref publication boundary. Never classify the exit
            // status alone as proof that the ref was unchanged.
            return Err(NativeGitIndexError::GitFailed {
                operation: "update-ref",
                status: update.status.to_string(),
            }
            .into_commit_boundary_unknown("update-ref"));
        }
        Ok(created_commit)
    }

    fn apply_hunks(
        &self,
        lock: &mut NativeIndexLock,
        preview: &GitIndexPreviewV1,
        patches: &[ValidatedIndexPatch],
        direction: HunkDirectionV1,
        reverse: bool,
    ) -> Result<(), NativeGitIndexError> {
        preview.validate()?;
        if !matches!(
            preview.disposition,
            GitIndexPreviewDispositionV1::Applicable
        ) || preview.operation.hunk_direction() != Some(direction)
        {
            return Err(NativeGitIndexError::PatchDoesNotMatchHunk);
        }
        self.require_lock(lock)?;
        self.verify_native_preconditions(lock, preview)?;
        let expected = preview.selected_hunk_digests()?;
        let actual: Result<Vec<ManifestDigest>, DomainError> = patches
            .iter()
            .map(|patch| patch.hunk().compute_digest())
            .collect();
        if expected != actual? {
            return Err(NativeGitIndexError::PatchDoesNotMatchHunk);
        }
        for patch in patches {
            self.verify_hunk_preconditions(patch.hunk())?;
        }
        let transaction =
            tempfile::tempdir().map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        let candidate_index = transaction.path().join("index");
        if self.index_path.exists() {
            std::fs::copy(&self.index_path, &candidate_index)
                .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        } else {
            File::create(&candidate_index)
                .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        }

        let patch_bytes = joined_patch_bytes(patches);
        let mut command = self.command();
        command
            .env("GIT_INDEX_FILE", &candidate_index)
            .arg("apply")
            .arg("--cached")
            .arg("--recount")
            .arg("--whitespace=nowarn")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if reverse {
            command.arg("--reverse");
        }
        run_command_with_stdin(command, "apply", &patch_bytes)?;

        let candidate_tree = self
            .command()
            .env("GIT_INDEX_FILE", &candidate_index)
            .arg("write-tree")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        if !candidate_tree.status.success() {
            return Err(NativeGitIndexError::GitFailed {
                operation: "apply write-tree",
                status: candidate_tree.status.to_string(),
            });
        }
        let candidate_tree = parse_git_oid("apply write-tree", &candidate_tree.stdout)?;
        if preview.candidate_index_tree.as_ref() != Some(&candidate_tree) {
            return Err(NativeGitIndexError::CandidateTreeMismatch);
        }

        // Candidate construction can take long enough for non-cooperating
        // worktree/ref writers to race us. Recheck every native and per-hunk
        // CAS fact again at the index publication boundary.
        self.verify_native_preconditions(lock, preview)?;
        for patch in patches {
            self.verify_hunk_preconditions(patch.hunk())?;
        }

        let candidate_bytes = std::fs::read(&candidate_index)
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        lock.file
            .rewind()
            .and_then(|()| lock.file.set_len(0))
            .and_then(|()| lock.file.write_all(&candidate_bytes))
            .and_then(|()| lock.file.sync_all())
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        // rename either publishes atomically or reports failure without
        // changing the destination. Durability becomes ambiguous only after a
        // successful rename when syncing the parent directory fails.
        std::fs::rename(&lock.path, &self.index_path)
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        lock.published = true;
        sync_parent_directory(&self.index_path)
            .map_err(|error| error.into_commit_boundary_unknown("index publish"))?;
        Ok(())
    }

    fn require_lock(&self, lock: &NativeIndexLock) -> Result<(), NativeGitIndexError> {
        if lock.path != self.index_lock_path() || !lock.path.is_file() || lock.published {
            return Err(NativeGitIndexError::IndexLocked);
        }
        Ok(())
    }

    fn verify_native_preconditions(
        &self,
        lock: &NativeIndexLock,
        preview: &GitIndexPreviewV1,
    ) -> Result<(), NativeGitIndexError> {
        let snapshot = &preview.repository_snapshot;
        let checksum = canonical_sha256(&self.index_bytes()?)?;
        if checksum != snapshot.index.checksum
            || self.index_tree_under_lock(lock).ok().as_ref() != snapshot.index.tree_id.as_ref()
            || self.refs_digest().ok().as_ref() != snapshot.refs_digest.as_ref()
            || self.git_version().ok().as_deref() != snapshot.git_version.as_deref()
            || snapshot.adapter_revision.as_deref() != Some(GIT_INDEX_ADAPTER_REVISION)
            || self.head_state().ok().as_ref() != Some(&snapshot.head)
            || current_operation_state(&self.git_dir) != snapshot.operation_state
        {
            return Err(NativeGitIndexError::StaleRepositoryState);
        }
        Ok(())
    }

    fn require_ref_value(
        &self,
        reference: &str,
        expected: &GitOidV1,
    ) -> Result<(), NativeGitIndexError> {
        let value = self.run_git("rev-parse", &["rev-parse", "--verify", reference])?;
        if parse_git_oid("rev-parse", &value.stdout)? != *expected {
            return Err(NativeGitIndexError::StaleRepositoryState);
        }
        Ok(())
    }

    fn write_tree_durable_under_lock(
        &self,
        lock: &NativeIndexLock,
    ) -> Result<GitOidV1, NativeGitIndexError> {
        self.require_lock(lock)?;
        let transaction =
            tempfile::tempdir().map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        let index = transaction.path().join("index");
        if self.index_path.exists() {
            std::fs::copy(&self.index_path, &index)
                .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        } else {
            File::create(&index).map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        }
        let output = self
            .command()
            .env("GIT_INDEX_FILE", &index)
            .arg("write-tree")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))?;
        if !output.status.success() {
            return Err(NativeGitIndexError::GitFailed {
                operation: "write-tree",
                status: output.status.to_string(),
            });
        }
        parse_git_oid("write-tree", &output.stdout)
    }

    fn verify_hunk_preconditions(&self, hunk: &HunkRefV1) -> Result<(), NativeGitIndexError> {
        let index_entry = self.index_entry(&hunk.path)?;
        if index_entry != hunk.expected_index_entry {
            return Err(NativeGitIndexError::StaleRepositoryState);
        }

        let base = match hunk.direction {
            HunkDirectionV1::WorkingTreeToIndex => index_entry.blob,
            HunkDirectionV1::IndexToHead => {
                self.head_blob(hunk.original_path.as_deref().unwrap_or(&hunk.path))?
            }
        };
        if base != hunk.expected_base_blob {
            return Err(NativeGitIndexError::StaleRepositoryState);
        }

        if let Some(expected_blob) = &hunk.expected_worktree_blob {
            let path = self.repository_root.join(&hunk.path);
            let actual_blob = if std::fs::symlink_metadata(&path).is_err() {
                GitBlobExpectationV1::AbsentFile
            } else {
                let output = self.run_git("hash-object", &["hash-object", "--", &hunk.path])?;
                GitBlobExpectationV1::Present(parse_git_oid("hash-object", &output.stdout)?)
            };
            if &actual_blob != expected_blob
                || worktree_mode(&path).as_ref() != hunk.expected_worktree_mode.as_ref()
            {
                return Err(NativeGitIndexError::StaleRepositoryState);
            }
        }

        let attributes =
            self.run_git("check-attr", &["check-attr", "-z", "-a", "--", &hunk.path])?;
        let attributes_digest =
            canonical_sha256(&String::from_utf8_lossy(&attributes.stdout).into_owned())?;
        if hunk.attributes_digest.as_ref() != Some(&attributes_digest) {
            return Err(NativeGitIndexError::StaleRepositoryState);
        }
        Ok(())
    }

    fn index_entry(&self, path: &str) -> Result<GitIndexEntryExpectationV1, NativeGitIndexError> {
        let output = self.run_git("ls-files", &["ls-files", "-s", "-z", "--", path])?;
        let text = String::from_utf8_lossy(&output.stdout);
        let mut entries = Vec::new();
        for record in text.split('\0').filter(|record| !record.is_empty()) {
            let (metadata, _) =
                record
                    .split_once('\t')
                    .ok_or(NativeGitIndexError::MalformedOutput {
                        operation: "ls-files",
                    })?;
            let mut fields = metadata.split_whitespace();
            let mode =
                GitFileModeV1::new(fields.next().ok_or(NativeGitIndexError::MalformedOutput {
                    operation: "ls-files",
                })?)?;
            let blob =
                GitOidV1::new(fields.next().ok_or(NativeGitIndexError::MalformedOutput {
                    operation: "ls-files",
                })?)?;
            let stage = fields
                .next()
                .and_then(|value| value.parse::<u8>().ok())
                .ok_or(NativeGitIndexError::MalformedOutput {
                    operation: "ls-files",
                })?;
            entries.push((mode, blob, stage));
        }
        let Some((mode, blob, _)) = entries.first().cloned() else {
            return Ok(GitIndexEntryExpectationV1 {
                blob: GitBlobExpectationV1::AbsentFile,
                mode: None,
                unmerged_stage: None,
            });
        };
        Ok(GitIndexEntryExpectationV1 {
            blob: GitBlobExpectationV1::Present(blob),
            mode: Some(mode),
            unmerged_stage: entries
                .iter()
                .map(|(_, _, stage)| *stage)
                .find(|stage| *stage > 0),
        })
    }

    fn head_blob(&self, path: &str) -> Result<GitBlobExpectationV1, NativeGitIndexError> {
        let output = self.run_git("ls-tree", &["ls-tree", "-z", "HEAD", "--", path])?;
        let text = String::from_utf8_lossy(&output.stdout);
        let Some(record) = text.split('\0').find(|record| !record.is_empty()) else {
            return Ok(GitBlobExpectationV1::AbsentFile);
        };
        let (metadata, _) =
            record
                .split_once('\t')
                .ok_or(NativeGitIndexError::MalformedOutput {
                    operation: "ls-tree",
                })?;
        let oid =
            metadata
                .split_whitespace()
                .nth(2)
                .ok_or(NativeGitIndexError::MalformedOutput {
                    operation: "ls-tree",
                })?;
        Ok(GitBlobExpectationV1::Present(GitOidV1::new(oid)?))
    }

    pub(crate) fn index_lock_path(&self) -> PathBuf {
        let mut name = self
            .index_path
            .file_name()
            .unwrap_or_else(|| OsStr::new("index"))
            .to_os_string();
        name.push(".lock");
        self.index_path.with_file_name(name)
    }

    fn ensure_no_applicable_hooks(&self) -> Result<(), NativeGitIndexError> {
        let configured = self.run_git_output(&["config", "--get", "core.hooksPath"])?;
        if configured.status.success() && !configured.stdout.is_empty() {
            return Err(NativeGitIndexError::UnsupportedHookPolicy);
        }
        if !configured.status.success() && configured.status.code() != Some(1) {
            return Err(NativeGitIndexError::GitFailed {
                operation: "config",
                status: configured.status.to_string(),
            });
        }
        for hook in [
            "pre-commit",
            "prepare-commit-msg",
            "commit-msg",
            "post-commit",
        ] {
            if is_executable_hook(&self.git_dir.join("hooks").join(hook)) {
                return Err(NativeGitIndexError::UnsupportedHookPolicy);
            }
        }
        Ok(())
    }

    fn command(&self) -> Command {
        git_command(&self.repository_root)
    }

    fn quarantine_command(&self, index_path: &Path, object_path: &Path) -> Command {
        let mut command = self.command();
        command
            .env("GIT_INDEX_FILE", index_path)
            .env("GIT_OBJECT_DIRECTORY", object_path)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &self.objects_path);
        command
    }

    fn run_git(
        &self,
        operation: &'static str,
        args: &[&str],
    ) -> Result<Output, NativeGitIndexError> {
        let output = self.run_git_output(args)?;
        if output.status.success() {
            Ok(output)
        } else {
            Err(NativeGitIndexError::GitFailed {
                operation,
                status: output.status.to_string(),
            })
        }
    }

    fn run_git_output(&self, args: &[&str]) -> Result<Output, NativeGitIndexError> {
        self.command()
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map_err(|error| NativeGitIndexError::Io(error.to_string()))
    }
}
