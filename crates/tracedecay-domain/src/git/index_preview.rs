//! Immutable Git index transaction intent and preview contracts.

use serde::{Deserialize, Deserializer, Serialize};

use crate::research::time::UtcMicros;
use crate::research::{DomainError, ManifestDigest, canonical_sha256};

use super::*;

/// The only native Git mutations represented by PR11. Generic Git execution,
/// ref rewrites, merge/rebase/cherry-pick, push, and worktree writes are
/// deliberately absent.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitIndexTransactionOperationV1 {
    StageHunks,
    UnstageHunks,
    CommitIndex,
}

impl GitIndexTransactionOperationV1 {
    pub const fn hunk_direction(self) -> Option<HunkDirectionV1> {
        match self {
            Self::StageHunks => Some(HunkDirectionV1::WorkingTreeToIndex),
            Self::UnstageHunks => Some(HunkDirectionV1::IndexToHead),
            Self::CommitIndex => None,
        }
    }
}

/// Why a preview is intentionally read-only. A caller must re-preview after
/// resolving the condition; no variant grants a relaxed or partial apply.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GitIndexUnsupportedStateV1 {
    BareRepository,
    DetachedHead,
    UnbornBranch,
    IndexLockPresent,
    ApplicableCommitHooks,
    SigningKeyUnavailable,
    UnmergedIndex,
    IntentToAdd,
    SplitIndex,
    SparseIndex,
    UnreadableIndex,
    ConflictedWorkingTree,
    UnreadableWorkingTree,
    InProgressOperation,
    UnsupportedObjectFormat,
    BinaryHunk,
    Submodule,
    Symlink,
    FileModeOnly,
    RenameOrCopy,
    FiltersOrEndOfLine,
    PartialHunkSelection,
}

/// Whether a captured preview may reach the daemon's native apply path.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum GitIndexPreviewDispositionV1 {
    Applicable,
    Unsupported(GitIndexUnsupportedStateV1),
}

impl GitIndexPreviewDispositionV1 {
    pub const fn is_applicable(&self) -> bool {
        matches!(self, Self::Applicable)
    }
}

/// The fixed commit-signing policy understood by `commit_index`. It is not a
/// generic collection of Git flags and does not authorize hook bypasses.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "policy")]
pub enum GitIndexSigningPolicyV1 {
    UnsignedPermitted,
    SignatureRequired { key_reference: String },
}

/// Structured, bounded commit input for the `commit_index` operation.
///
/// The message is retained only while the native transaction is in flight;
/// previews and durable receipts retain its digest rather than the text.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitIndexCommitIntentV1 {
    pub message: String,
    pub message_digest: ManifestDigest,
    pub author: GitCommitIdentityV1,
    pub committer: GitCommitIdentityV1,
    pub signing_policy: GitIndexSigningPolicyV1,
}

#[derive(Serialize)]
struct GitIndexCommitIntentDigestMaterial<'a> {
    domain: &'static str,
    message_digest: &'a ManifestDigest,
    author: &'a GitCommitIdentityV1,
    committer: &'a GitCommitIdentityV1,
    signing_policy: &'a GitIndexSigningPolicyV1,
}

impl GitIndexCommitIntentV1 {
    pub fn new(
        message: String,
        author: GitCommitIdentityV1,
        committer: GitCommitIdentityV1,
        signing_policy: GitIndexSigningPolicyV1,
    ) -> Result<Self, DomainError> {
        let mut intent = Self {
            message,
            message_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))?,
            author,
            committer,
            signing_policy,
        };
        intent.message_digest = intent.compute_message_digest()?;
        intent.validate()?;
        Ok(intent)
    }

    pub fn compute_message_digest(&self) -> Result<ManifestDigest, DomainError> {
        validate_git_commit_message(&self.message)?;
        canonical_sha256(&("tracedecay.git-index.commit-message.v1", &self.message))
    }

    /// Commit to every canonical intent field without retaining plaintext
    /// commit material in a preview or durable transaction record. Git stores
    /// author and committer timestamps at whole-second precision, so the
    /// digest uses the same canonical representation without changing the
    /// request's wire-visible identity values.
    pub fn compute_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate()?;
        let author = canonical_git_commit_identity(&self.author)?;
        let committer = canonical_git_commit_identity(&self.committer)?;
        canonical_sha256(&GitIndexCommitIntentDigestMaterial {
            domain: GIT_INDEX_COMMIT_INTENT_DIGEST_DOMAIN_V1,
            message_digest: &self.message_digest,
            author: &author,
            committer: &committer,
            signing_policy: &self.signing_policy,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        validate_git_commit_message(&self.message)?;
        if self.message_digest != self.compute_message_digest()? {
            return Err(DomainError::DigestMismatch);
        }
        validate_git_commit_identity(&self.author)?;
        validate_git_commit_identity(&self.committer)?;
        if let GitIndexSigningPolicyV1::SignatureRequired { key_reference } = &self.signing_policy {
            validate_path_label(key_reference, "git index signing key reference")?;
        }
        Ok(())
    }
}

fn canonical_git_commit_identity(
    identity: &GitCommitIdentityV1,
) -> Result<GitCommitIdentityV1, DomainError> {
    let seconds = identity.at.0.div_euclid(1_000_000);
    let micros = seconds
        .checked_mul(1_000_000)
        .ok_or(DomainError::NonCanonical {
            field: "git commit identity timestamp",
        })?;
    let mut canonical = identity.clone();
    canonical.at = UtcMicros(micros);
    Ok(canonical)
}

fn validate_git_commit_message(message: &str) -> Result<(), DomainError> {
    if message.is_empty() {
        return Err(DomainError::Empty {
            field: "git index commit message",
        });
    }
    if message.len() > 65_536 || message.contains('\0') {
        return Err(DomainError::NonCanonical {
            field: "git index commit message",
        });
    }
    Ok(())
}

fn validate_git_commit_identity(identity: &GitCommitIdentityV1) -> Result<(), DomainError> {
    validate_path_label(&identity.name, "git index commit identity name")?;
    validate_path_label(&identity.email, "git index commit identity email")
}

/// Immutable, content-bound preview for one daemon-serialized index
/// transaction. Applicability is only a precondition: the daemon must capture
/// and compare the entire snapshot and every contained `HunkRefV1` again
/// immediately before a native mutation.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitIndexPreviewV1 {
    pub preview_id: GitIndexPreviewId,
    pub operation: GitIndexTransactionOperationV1,
    pub repository_snapshot: RepositoryStateSnapshotV1,
    pub repository_snapshot_digest: ManifestDigest,
    pub selected_hunks: Vec<HunkRefV1>,
    pub candidate_index_tree: Option<GitOidV1>,
    /// Canonical commitment to the full commit input. It is present exactly
    /// for `commit_index`; plaintext message, identity, timestamp, key, and
    /// signing policy remain process-local ephemeral material.
    pub commit_intent_digest: Option<ManifestDigest>,
    pub disposition: GitIndexPreviewDispositionV1,
    pub created_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub preview_digest: ManifestDigest,
}

#[derive(Serialize)]
struct GitIndexPreviewDigestMaterial<'a> {
    domain: &'static str,
    preview_id: &'a GitIndexPreviewId,
    operation: GitIndexTransactionOperationV1,
    repository_snapshot_id: &'a RepositoryStateSnapshotId,
    repository_snapshot_digest: &'a ManifestDigest,
    selected_hunk_digests: &'a [ManifestDigest],
    candidate_index_tree: Option<&'a GitOidV1>,
    commit_intent_digest: Option<&'a ManifestDigest>,
    disposition: &'a GitIndexPreviewDispositionV1,
    created_at: UtcMicros,
    expires_at: UtcMicros,
}

impl GitIndexPreviewV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        preview_id: GitIndexPreviewId,
        operation: GitIndexTransactionOperationV1,
        repository_snapshot: RepositoryStateSnapshotV1,
        repository_snapshot_digest: ManifestDigest,
        selected_hunks: Vec<HunkRefV1>,
        candidate_index_tree: Option<GitOidV1>,
        disposition: GitIndexPreviewDispositionV1,
        created_at: UtcMicros,
        expires_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        Self::new_with_commit_intent(
            preview_id,
            operation,
            repository_snapshot,
            repository_snapshot_digest,
            selected_hunks,
            candidate_index_tree,
            None,
            disposition,
            created_at,
            expires_at,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_commit_intent(
        preview_id: GitIndexPreviewId,
        operation: GitIndexTransactionOperationV1,
        repository_snapshot: RepositoryStateSnapshotV1,
        repository_snapshot_digest: ManifestDigest,
        selected_hunks: Vec<HunkRefV1>,
        candidate_index_tree: Option<GitOidV1>,
        commit_intent: Option<&GitIndexCommitIntentV1>,
        disposition: GitIndexPreviewDispositionV1,
        created_at: UtcMicros,
        expires_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        let commit_intent_digest = commit_intent
            .map(GitIndexCommitIntentV1::compute_digest)
            .transpose()?;
        Self::new_with_commit_intent_digest(
            preview_id,
            operation,
            repository_snapshot,
            repository_snapshot_digest,
            selected_hunks,
            candidate_index_tree,
            commit_intent_digest,
            disposition,
            created_at,
            expires_at,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_commit_intent_digest(
        preview_id: GitIndexPreviewId,
        operation: GitIndexTransactionOperationV1,
        repository_snapshot: RepositoryStateSnapshotV1,
        repository_snapshot_digest: ManifestDigest,
        selected_hunks: Vec<HunkRefV1>,
        candidate_index_tree: Option<GitOidV1>,
        commit_intent_digest: Option<ManifestDigest>,
        disposition: GitIndexPreviewDispositionV1,
        created_at: UtcMicros,
        expires_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        let mut preview = Self {
            preview_id,
            operation,
            repository_snapshot,
            repository_snapshot_digest,
            selected_hunks,
            candidate_index_tree,
            commit_intent_digest,
            disposition,
            created_at,
            expires_at,
            preview_digest: ManifestDigest::new(format!("sha256:{}", "0".repeat(64)))?,
        };
        preview.preview_digest = preview.compute_preview_digest()?;
        preview.validate()?;
        Ok(preview)
    }

    pub fn repository_snapshot_digest(
        snapshot: &RepositoryStateSnapshotV1,
    ) -> Result<ManifestDigest, DomainError> {
        snapshot.validate()?;
        canonical_sha256(&(GIT_INDEX_SNAPSHOT_DIGEST_DOMAIN_V1, snapshot))
    }

    pub fn selected_hunk_digests(&self) -> Result<Vec<ManifestDigest>, DomainError> {
        self.selected_hunks
            .iter()
            .map(HunkRefV1::compute_digest)
            .collect()
    }

    pub fn is_expired_at(&self, observed_at: UtcMicros) -> bool {
        observed_at >= self.expires_at
    }

    pub fn compute_preview_digest(&self) -> Result<ManifestDigest, DomainError> {
        self.validate_fields()?;
        let hunk_digests = self.selected_hunk_digests()?;
        canonical_sha256(&GitIndexPreviewDigestMaterial {
            domain: GIT_INDEX_PREVIEW_DIGEST_DOMAIN_V1,
            preview_id: &self.preview_id,
            operation: self.operation,
            repository_snapshot_id: self.repository_snapshot.snapshot_id(),
            repository_snapshot_digest: &self.repository_snapshot_digest,
            selected_hunk_digests: &hunk_digests,
            candidate_index_tree: self.candidate_index_tree.as_ref(),
            commit_intent_digest: self.commit_intent_digest.as_ref(),
            disposition: &self.disposition,
            created_at: self.created_at,
            expires_at: self.expires_at,
        })
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.preview_digest.validate()?;
        self.validate_fields()?;
        if self.preview_digest != self.compute_preview_digest()? {
            return Err(DomainError::DigestMismatch);
        }
        Ok(())
    }

    fn validate_fields(&self) -> Result<(), DomainError> {
        self.preview_id.validate()?;
        self.repository_snapshot.validate()?;
        self.repository_snapshot_digest.validate()?;
        if self.repository_snapshot_digest
            != Self::repository_snapshot_digest(&self.repository_snapshot)?
        {
            return Err(DomainError::SnapshotMismatch {
                field: "git index preview repository snapshot digest",
            });
        }
        if self.expires_at <= self.created_at {
            return Err(DomainError::InvalidTimeInterval);
        }

        let mut hunk_digests = Vec::with_capacity(self.selected_hunks.len());
        for hunk in &self.selected_hunks {
            hunk.validate()?;
            if hunk.repository != self.repository_snapshot.repository_id
                || self.repository_snapshot.worktree_id.as_ref() != Some(&hunk.worktree)
                || hunk.preview_id != self.preview_id.as_str()
                || hunk.snapshot_digest != self.repository_snapshot_digest
            {
                return Err(DomainError::SnapshotMismatch {
                    field: "git index preview hunk compare-and-swap binding",
                });
            }
            if self.operation.hunk_direction() != Some(hunk.direction) {
                return Err(DomainError::NonCanonical {
                    field: "git index preview hunk direction",
                });
            }
            hunk_digests.push(hunk.compute_digest()?);
        }
        if hunk_digests.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(DomainError::DuplicateId {
                field: "git index preview hunk digest order",
            });
        }

        if let Some(tree) = &self.candidate_index_tree {
            tree.validate()?;
            if tree.format() != self.repository_snapshot.object_format {
                return Err(DomainError::NonCanonical {
                    field: "git index preview candidate tree format",
                });
            }
        }
        if let Some(intent_digest) = &self.commit_intent_digest {
            intent_digest.validate()?;
        }

        match (&self.disposition, self.operation) {
            (
                GitIndexPreviewDispositionV1::Applicable,
                GitIndexTransactionOperationV1::CommitIndex,
            ) => {
                if !self.repository_snapshot.is_mutation_eligible()
                    || !matches!(
                        self.repository_snapshot.head,
                        GitHeadStateV1::Attached { .. }
                    )
                    || !self.selected_hunks.is_empty()
                    || self.commit_intent_digest.is_none()
                    || self.candidate_index_tree.as_ref()
                        != self.repository_snapshot.index.tree_id.as_ref()
                {
                    return Err(DomainError::NonCanonical {
                        field: "applicable git index commit preview",
                    });
                }
            }
            (GitIndexPreviewDispositionV1::Applicable, _) => {
                if !self.repository_snapshot.is_mutation_eligible()
                    || self.selected_hunks.is_empty()
                    || self.commit_intent_digest.is_some()
                    || self.candidate_index_tree.is_none()
                {
                    return Err(DomainError::NonCanonical {
                        field: "applicable git index hunk preview",
                    });
                }
            }
            (GitIndexPreviewDispositionV1::Unsupported(_), _) => {
                if !self.selected_hunks.is_empty()
                    || self.candidate_index_tree.is_some()
                    || (self.operation == GitIndexTransactionOperationV1::CommitIndex)
                        != self.commit_intent_digest.is_some()
                {
                    return Err(DomainError::NonCanonical {
                        field: "unsupported git index preview mutation payload",
                    });
                }
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for GitIndexPreviewV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            preview_id: GitIndexPreviewId,
            operation: GitIndexTransactionOperationV1,
            repository_snapshot: RepositoryStateSnapshotV1,
            repository_snapshot_digest: ManifestDigest,
            selected_hunks: Vec<HunkRefV1>,
            candidate_index_tree: Option<GitOidV1>,
            commit_intent_digest: Option<ManifestDigest>,
            disposition: GitIndexPreviewDispositionV1,
            created_at: UtcMicros,
            expires_at: UtcMicros,
            preview_digest: ManifestDigest,
        }

        let wire = Wire::deserialize(deserializer)?;
        let preview = Self::new_with_commit_intent_digest(
            wire.preview_id,
            wire.operation,
            wire.repository_snapshot,
            wire.repository_snapshot_digest,
            wire.selected_hunks,
            wire.candidate_index_tree,
            wire.commit_intent_digest,
            wire.disposition,
            wire.created_at,
            wire.expires_at,
        )
        .map_err(serde::de::Error::custom)?;
        if preview.preview_digest != wire.preview_digest {
            return Err(serde::de::Error::custom(
                "git index preview digest does not match its immutable payload",
            ));
        }
        Ok(preview)
    }
}
