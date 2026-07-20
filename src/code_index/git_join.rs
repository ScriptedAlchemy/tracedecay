//! Generation-exact joins over Plan 36 read-only Git evidence.
//!
//! Native Git remains authoritative for status, diff, hunk, blob, mode, and
//! coverage semantics. This module only verifies that a typed [`GitDiffV1`]
//! and its capture watermark describe the exact sanitized snapshot sealed by
//! one code generation, then attaches canonical file occurrence/content
//! identity. It never reads a repository, reconstructs a patch, or infers a
//! match from a path or line alone.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, CommitId, ContentDigest, FileOccurrenceId,
    GitChangeKindV1, GitDegradationV1, GitDiffScopeV1, GitDiffV1, GitFileModeV1, GitHunkV1,
    GitOidV1, ManifestDigest, RepositoryId, SanitizedCodeFileV1, SnapshotFileDispositionV1,
    UtcMicros, ValidatedCodeSnapshotV1, WorktreeId,
};

use super::capabilities::expected_seal_digest;

/// Plan-36/capture watermark retained separately from the code-generation
/// watermark. Equality against the sanitized snapshot is required before any
/// file or hunk evidence can be attached.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitWatermarkV1 {
    pub repository: RepositoryId,
    pub worktree: Option<WorktreeId>,
    pub source_revision: Option<CommitId>,
    pub snapshot_content_identity: ContentDigest,
    /// Independent digest of the native Git/capture observation. This is
    /// preserved as provenance and is not substituted for the code snapshot
    /// digest.
    pub git_snapshot_digest: ManifestDigest,
    pub captured_at: UtcMicros,
}

/// Exact content identity observed for one path by the Git/capture boundary.
/// The join requires this digest to equal the sanitized snapshot file digest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitFileContentIdentityV1 {
    pub path: String,
    pub content_digest: ContentDigest,
}

/// Why a file can retain exact file-level Git evidence but cannot expose
/// text-hunk/symbol attachment.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum GenerationGitFileOnlyReasonV1 {
    Binary,
    Submodule,
}

/// Per-file join state. Both variants have exact generation, path, and
/// content identity; `FileOnly` prevents binary/submodule evidence from being
/// mistaken for source-range evidence.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GenerationGitFileJoinStateV1 {
    Exact,
    FileOnly {
        reason: GenerationGitFileOnlyReasonV1,
    },
}

/// Typed reasons a generation-exact Git join has incomplete source-range
/// coverage. Partial never means clean.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum GenerationGitPartialReasonV1 {
    GitDegraded { degradation: GitDegradationV1 },
    BinaryFile { path: String },
    Submodule { path: String },
}

/// Overall Git join coverage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum GenerationGitJoinCoverageV1 {
    Complete,
    Partial {
        reasons: Vec<GenerationGitPartialReasonV1>,
    },
}

/// One Plan-36 file diff attached to exact code-generation file identity.
/// Native Git fields are preserved, not recomputed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitFileJoinV1 {
    pub path: String,
    pub original_path: Option<String>,
    pub change: GitChangeKindV1,
    pub file_occurrence_id: FileOccurrenceId,
    pub content_digest: ContentDigest,
    pub old_mode: Option<GitFileModeV1>,
    pub new_mode: Option<GitFileModeV1>,
    pub old_blob: Option<GitOidV1>,
    pub new_blob: Option<GitOidV1>,
    pub binary: bool,
    pub submodule: bool,
    pub hunks: Vec<GitHunkV1>,
    pub join_state: GenerationGitFileJoinStateV1,
}

/// Generation-bound view of one read-only Git diff.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitJoinV1 {
    pub generation_id: CodeGenerationId,
    /// Code-index generation watermark.
    pub code_snapshot_digest: ManifestDigest,
    pub code_content_identity: ContentDigest,
    /// Independent Plan-36/capture watermark.
    pub git_watermark: GenerationGitWatermarkV1,
    pub scope: GitDiffScopeV1,
    pub files: Vec<GenerationGitFileJoinV1>,
    pub coverage: GenerationGitJoinCoverageV1,
}

/// Typed refusal to combine stale, mixed, or non-exact evidence.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum GenerationGitJoinErrorV1 {
    #[error("the code generation does not seal the supplied sanitized snapshot")]
    StaleGenerationWatermark,
    #[error("the Git evidence names another repository")]
    RepositoryMismatch,
    #[error("the Git/capture watermark names another worktree")]
    WorktreeMismatch,
    #[error("the Git/capture watermark names another source revision")]
    StaleSourceRevision,
    #[error("the Git/capture content watermark is stale")]
    StaleContentWatermark,
    #[error("duplicate Git content identity for {0}")]
    DuplicateContentIdentity(String),
    #[error("Git evidence for {0} has no exact content identity")]
    MissingContentIdentity(String),
    #[error("Git evidence for {0} has no file in the generation snapshot")]
    MissingSnapshotFile(String),
    #[error("Git and generation content identity differ for {0}")]
    ContentMismatch(String),
    #[error("Git change kind and snapshot disposition differ for {0}")]
    DispositionMismatch(String),
    #[error("invalid generation or Git evidence: {0}")]
    Contract(String),
}

impl GenerationGitJoinV1 {
    /// Bind one typed Plan-36 diff to one immutable code generation.
    ///
    /// Path lookup is only an index into independently supplied evidence:
    /// repository/worktree/source/content watermarks and each file's content
    /// digest must match exactly before the result is emitted.
    pub fn join(
        generation: &CodeGenerationManifestV1,
        snapshot: &ValidatedCodeSnapshotV1,
        diff: &GitDiffV1,
        git_watermark: &GenerationGitWatermarkV1,
        file_contents: &[GitFileContentIdentityV1],
    ) -> Result<Self, GenerationGitJoinErrorV1> {
        validate_generation_snapshot(generation, snapshot)?;
        validate_git_watermark(snapshot, diff, git_watermark)?;
        diff.validate()
            .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;

        let content_by_path = index_content_identity(file_contents)?;
        let snapshot_by_path: BTreeMap<&str, &SanitizedCodeFileV1> = snapshot
            .snapshot
            .files
            .iter()
            .map(|file| (file.logical_path.as_str(), file))
            .collect();

        let mut files = Vec::with_capacity(diff.files.len());
        let mut partial_reasons: Vec<GenerationGitPartialReasonV1> = diff
            .coverage
            .degradations
            .iter()
            .copied()
            .map(|degradation| GenerationGitPartialReasonV1::GitDegraded { degradation })
            .collect();

        for git_file in &diff.files {
            let snapshot_file = snapshot_by_path
                .get(git_file.path.as_str())
                .copied()
                .ok_or_else(|| {
                    GenerationGitJoinErrorV1::MissingSnapshotFile(git_file.path.clone())
                })?;
            let observed_content =
                content_by_path.get(git_file.path.as_str()).ok_or_else(|| {
                    GenerationGitJoinErrorV1::MissingContentIdentity(git_file.path.clone())
                })?;
            if *observed_content != &snapshot_file.content_digest {
                return Err(GenerationGitJoinErrorV1::ContentMismatch(
                    git_file.path.clone(),
                ));
            }
            if !disposition_matches(git_file.change, snapshot_file.disposition, git_file.binary) {
                return Err(GenerationGitJoinErrorV1::DispositionMismatch(
                    git_file.path.clone(),
                ));
            }

            let join_state = if git_file.binary {
                partial_reasons.push(GenerationGitPartialReasonV1::BinaryFile {
                    path: git_file.path.clone(),
                });
                GenerationGitFileJoinStateV1::FileOnly {
                    reason: GenerationGitFileOnlyReasonV1::Binary,
                }
            } else if git_file.submodule {
                partial_reasons.push(GenerationGitPartialReasonV1::Submodule {
                    path: git_file.path.clone(),
                });
                GenerationGitFileJoinStateV1::FileOnly {
                    reason: GenerationGitFileOnlyReasonV1::Submodule,
                }
            } else {
                GenerationGitFileJoinStateV1::Exact
            };

            files.push(GenerationGitFileJoinV1 {
                path: git_file.path.clone(),
                original_path: git_file.original_path.clone(),
                change: git_file.change,
                file_occurrence_id: snapshot_file.file_occurrence_id.clone(),
                content_digest: snapshot_file.content_digest.clone(),
                old_mode: git_file.old_mode.clone(),
                new_mode: git_file.new_mode.clone(),
                old_blob: git_file.old_blob.clone(),
                new_blob: git_file.new_blob.clone(),
                binary: git_file.binary,
                submodule: git_file.submodule,
                hunks: git_file.hunks.clone(),
                join_state,
            });
        }
        files.sort_by(|left, right| left.path.cmp(&right.path));
        partial_reasons.sort();
        partial_reasons.dedup();
        let coverage = if partial_reasons.is_empty() {
            GenerationGitJoinCoverageV1::Complete
        } else {
            GenerationGitJoinCoverageV1::Partial {
                reasons: partial_reasons,
            }
        };

        Ok(Self {
            generation_id: generation.generation_id.clone(),
            code_snapshot_digest: generation.snapshot_digest.clone(),
            code_content_identity: snapshot.snapshot.content_identity.clone(),
            git_watermark: git_watermark.clone(),
            scope: diff.scope.clone(),
            files,
            coverage,
        })
    }
}

fn validate_generation_snapshot(
    generation: &CodeGenerationManifestV1,
    snapshot: &ValidatedCodeSnapshotV1,
) -> Result<(), GenerationGitJoinErrorV1> {
    snapshot
        .snapshot
        .validate()
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
    if generation.snapshot_digest != snapshot.intake_digest {
        return Err(GenerationGitJoinErrorV1::StaleGenerationWatermark);
    }
    generation
        .validate()
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
    let seal = expected_seal_digest(generation)
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
    if seal != generation.seal.expected_digest {
        return Err(GenerationGitJoinErrorV1::StaleGenerationWatermark);
    }
    Ok(())
}

fn validate_git_watermark(
    snapshot: &ValidatedCodeSnapshotV1,
    diff: &GitDiffV1,
    watermark: &GenerationGitWatermarkV1,
) -> Result<(), GenerationGitJoinErrorV1> {
    watermark
        .repository
        .validate()
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
    watermark
        .snapshot_content_identity
        .validate()
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
    watermark
        .git_snapshot_digest
        .validate()
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
    if watermark.repository != snapshot.snapshot.repository
        || diff.repository != snapshot.snapshot.repository
    {
        return Err(GenerationGitJoinErrorV1::RepositoryMismatch);
    }
    if watermark.worktree != snapshot.snapshot.worktree {
        return Err(GenerationGitJoinErrorV1::WorktreeMismatch);
    }
    if watermark.source_revision != snapshot.snapshot.source_revision {
        return Err(GenerationGitJoinErrorV1::StaleSourceRevision);
    }
    if watermark.snapshot_content_identity != snapshot.snapshot.content_identity {
        return Err(GenerationGitJoinErrorV1::StaleContentWatermark);
    }
    Ok(())
}

fn index_content_identity(
    identities: &[GitFileContentIdentityV1],
) -> Result<BTreeMap<&str, &ContentDigest>, GenerationGitJoinErrorV1> {
    let mut by_path = BTreeMap::new();
    for identity in identities {
        identity
            .content_digest
            .validate()
            .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
        if identity.path.is_empty() {
            return Err(GenerationGitJoinErrorV1::Contract(
                "empty Git content-identity path".to_owned(),
            ));
        }
        if by_path
            .insert(identity.path.as_str(), &identity.content_digest)
            .is_some()
        {
            return Err(GenerationGitJoinErrorV1::DuplicateContentIdentity(
                identity.path.clone(),
            ));
        }
    }
    Ok(by_path)
}

fn disposition_matches(
    change: GitChangeKindV1,
    disposition: SnapshotFileDispositionV1,
    binary: bool,
) -> bool {
    if binary {
        return disposition == SnapshotFileDispositionV1::Binary;
    }
    match change {
        GitChangeKindV1::Deleted => disposition == SnapshotFileDispositionV1::Deleted,
        GitChangeKindV1::Renamed | GitChangeKindV1::Copied => matches!(
            disposition,
            SnapshotFileDispositionV1::Renamed | SnapshotFileDispositionV1::Present
        ),
        GitChangeKindV1::Unmodified
        | GitChangeKindV1::Modified
        | GitChangeKindV1::Added
        | GitChangeKindV1::TypeChanged
        | GitChangeKindV1::Unmerged => disposition == SnapshotFileDispositionV1::Present,
    }
}
