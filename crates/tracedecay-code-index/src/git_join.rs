//! Generation-exact joins over Plan 36 read-only Git evidence.
//!
//! Native Git remains authoritative for status, diff, hunk, history, blame,
//! blob, mode, and coverage semantics. This module only verifies that typed
//! Git results and their capture watermarks describe the exact sanitized
//! snapshot sealed by one code generation, then attaches canonical occurrence
//! and content identity. It never reads a repository, reconstructs a patch, or
//! infers Git evidence from indexed rows.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, CommitId, ContentDigest, FileOccurrenceId,
    GitBlameAvailabilityV1, GitBlameLineV1, GitBlameV1, GitChangeKindV1, GitDegradationV1,
    GitDiffScopeV1, GitDiffV1, GitFileModeV1, GitHistoryV1, GitHunkV1, GitOidV1, ManifestDigest,
    RefId, RepositoryId, SanitizedCodeFileV1, SnapshotFileDispositionV1, SymbolOccurrenceId,
    UtcMicros, ValidatedCodeSnapshotV1, WorktreeId, canonical_sha256,
};

use super::capabilities::expected_seal_digest;
use super::diagnostics::{GenerationDiagnosticDispositionV1, GenerationDiagnosticJoinV1};
use super::impact_join::GenerationImpactJoinV1;
use super::provider::GenerationProviderReadV1;
use super::test_attribution::{GenerationTestJoinDispositionV1, GenerationTestJoinV1};

const GIT_JOIN_EVIDENCE_SEPARATOR: &str = "tracedecay.generation-git-evidence.v1";
const GIT_HISTORY_EVIDENCE_SEPARATOR: &str = "tracedecay.generation-git-history.v1";
const GIT_BLAME_EVIDENCE_SEPARATOR: &str = "tracedecay.generation-git-blame.v1";

/// Immutable native-Git scope captured with a generation join.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitEvidenceScopeV1 {
    pub worktree: Option<WorktreeId>,
    pub index_tree: Option<GitOidV1>,
    pub tree: Option<GitOidV1>,
    pub reference: Option<RefId>,
    pub options_digest: ManifestDigest,
}

impl GenerationGitEvidenceScopeV1 {
    fn validate(&self) -> Result<(), GenerationGitJoinErrorV1> {
        if let Some(worktree) = &self.worktree {
            worktree
                .validate()
                .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
        }
        if let Some(index_tree) = &self.index_tree {
            index_tree
                .validate()
                .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
        }
        if let Some(tree) = &self.tree {
            tree.validate()
                .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
        }
        if let Some(reference) = &self.reference {
            reference
                .validate()
                .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
        }
        self.options_digest
            .validate()
            .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))
    }
}

#[derive(Serialize)]
struct GenerationGitEvidenceDigestInput<'a> {
    domain: &'static str,
    repository: &'a RepositoryId,
    source_revision: &'a Option<CommitId>,
    snapshot_content_identity: &'a ContentDigest,
    scope: &'a GenerationGitEvidenceScopeV1,
    diff_scope: &'a GitDiffScopeV1,
    diff: &'a GitDiffV1,
}

#[derive(Serialize)]
struct GenerationGitReadEvidenceDigestInput<'a, T> {
    domain: &'static str,
    repository: &'a RepositoryId,
    source_revision: &'a Option<CommitId>,
    snapshot_content_identity: &'a ContentDigest,
    scope: &'a GenerationGitEvidenceScopeV1,
    evidence: &'a T,
}

/// Plan-36/capture watermark retained separately from the code-generation
/// watermark. Equality against the sanitized snapshot is required before any
/// file or hunk evidence can be attached.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitWatermarkV1 {
    pub repository: RepositoryId,
    pub source_revision: Option<CommitId>,
    pub snapshot_content_identity: ContentDigest,
    /// Exact worktree/index/tree/ref/options evidence captured by native Git.
    pub scope: GenerationGitEvidenceScopeV1,
    /// The typed diff operation whose options and hunk sides are sealed into
    /// `git_snapshot_digest`.
    pub diff_scope: GitDiffScopeV1,
    /// Canonical digest of the immutable scope and complete typed native diff.
    /// It binds both old and new blob/path/range hunk sides.
    pub git_snapshot_digest: ManifestDigest,
    pub captured_at: UtcMicros,
}

impl GenerationGitWatermarkV1 {
    pub fn recompute_evidence_digest(
        &self,
        diff: &GitDiffV1,
    ) -> Result<ManifestDigest, GenerationGitJoinErrorV1> {
        self.scope.validate()?;
        canonical_sha256(&GenerationGitEvidenceDigestInput {
            domain: GIT_JOIN_EVIDENCE_SEPARATOR,
            repository: &self.repository,
            source_revision: &self.source_revision,
            snapshot_content_identity: &self.snapshot_content_identity,
            scope: &self.scope,
            diff_scope: &self.diff_scope,
            diff,
        })
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))
    }
}

/// Independent watermark for native Git history or blame reads.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitReadWatermarkV1 {
    pub repository: RepositoryId,
    pub source_revision: Option<CommitId>,
    pub snapshot_content_identity: ContentDigest,
    pub scope: GenerationGitEvidenceScopeV1,
    pub evidence_digest: ManifestDigest,
    pub captured_at: UtcMicros,
}

impl GenerationGitReadWatermarkV1 {
    pub fn recompute_history_digest(
        &self,
        history: &GitHistoryV1,
    ) -> Result<ManifestDigest, GenerationGitJoinErrorV1> {
        self.recompute_digest(GIT_HISTORY_EVIDENCE_SEPARATOR, history)
    }

    pub fn recompute_blame_digest(
        &self,
        blame: &GitBlameV1,
    ) -> Result<ManifestDigest, GenerationGitJoinErrorV1> {
        self.recompute_digest(GIT_BLAME_EVIDENCE_SEPARATOR, blame)
    }

    fn recompute_digest<T: Serialize>(
        &self,
        domain: &'static str,
        evidence: &T,
    ) -> Result<ManifestDigest, GenerationGitJoinErrorV1> {
        self.scope.validate()?;
        canonical_sha256(&GenerationGitReadEvidenceDigestInput {
            domain,
            repository: &self.repository,
            source_revision: &self.source_revision,
            snapshot_content_identity: &self.snapshot_content_identity,
            scope: &self.scope,
            evidence,
        })
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))
    }
}

/// Exact content identity observed for one path by the Git/capture boundary.
/// The join requires this digest to equal the sanitized snapshot file digest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitFileContentIdentityV1 {
    pub path: String,
    pub content_digest: ContentDigest,
}

/// Native history joined to one immutable code-generation watermark.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum GenerationGitHistoryJoinCoverageV1 {
    Complete,
    Partial {
        degradations: Vec<GitDegradationV1>,
        truncated: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitHistoryJoinV1 {
    pub generation_id: CodeGenerationId,
    pub code_snapshot_digest: ManifestDigest,
    pub code_content_identity: ContentDigest,
    pub git_watermark: GenerationGitReadWatermarkV1,
    pub history: GitHistoryV1,
    pub coverage: GenerationGitHistoryJoinCoverageV1,
}

/// Exact generation-local symbol line range supplied by the canonical code
/// occurrence authority. Native blame remains line provenance authority.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct GitSymbolLineBindingV1 {
    pub generation_id: CodeGenerationId,
    pub file_occurrence_id: FileOccurrenceId,
    pub symbol_occurrence_id: SymbolOccurrenceId,
    pub content_digest: ContentDigest,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "coverage", rename_all = "snake_case")]
pub enum GenerationGitBlameJoinCoverageV1 {
    Complete,
    Partial {
        degradations: Vec<GitDegradationV1>,
    },
    Unavailable {
        availability: GitBlameAvailabilityV1,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitBlameLineJoinV1 {
    pub line: GitBlameLineV1,
    pub symbol_occurrence_ids: Vec<SymbolOccurrenceId>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitBlameJoinV1 {
    pub generation_id: CodeGenerationId,
    pub code_snapshot_digest: ManifestDigest,
    pub code_content_identity: ContentDigest,
    pub git_watermark: GenerationGitReadWatermarkV1,
    pub file_occurrence_id: FileOccurrenceId,
    pub content_digest: ContentDigest,
    pub blame: GitBlameV1,
    pub lines: Vec<GenerationGitBlameLineJoinV1>,
    pub coverage: GenerationGitBlameJoinCoverageV1,
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
    /// Exact generation-local context for each text hunk, in `hunks` order.
    /// Binary and submodule files never carry hunk context.
    #[serde(default)]
    pub hunk_contexts: Vec<GenerationGitHunkContextV1>,
    pub join_state: GenerationGitFileJoinStateV1,
}

/// Graph/test context for one exact native-Git hunk.
///
/// Git proves only the changed line range. Symbol identity comes from the code
/// occurrence authority, while callers, hazard anchors, and affected tests
/// retain the graph/test provider state embedded in each impact read.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GenerationGitHunkContextV1 {
    pub patch_digest: ManifestDigest,
    pub symbol_occurrence_ids: Vec<SymbolOccurrenceId>,
    pub impacts: Vec<GenerationProviderReadV1<GenerationImpactJoinV1>>,
    pub diagnostic_anchors: Vec<tracedecay_domain::RetrievalAnchorId>,
    pub hazard_anchors: Vec<tracedecay_domain::RetrievalAnchorId>,
    pub affected_tests: Vec<SymbolOccurrenceId>,
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
    /// Plan-35 evidence is independently evaluated and never inherits Git
    /// freshness or completeness.
    #[serde(default)]
    pub diagnostics: Option<GenerationProviderReadV1<GenerationDiagnosticJoinV1>>,
    /// Test-map evidence is independently evaluated and never upgrades a hunk
    /// match to executed-test proof.
    #[serde(default)]
    pub test_attribution: Option<GenerationProviderReadV1<GenerationTestJoinV1>>,
    pub coverage: GenerationGitJoinCoverageV1,
}

/// Independently sourced context used to enrich an exact Git diff.
#[derive(Clone, Debug)]
pub struct GenerationGitContextProvidersV1 {
    pub symbol_bindings: Vec<GitSymbolLineBindingV1>,
    pub impacts: Vec<(
        SymbolOccurrenceId,
        GenerationProviderReadV1<GenerationImpactJoinV1>,
    )>,
    pub diagnostics: GenerationProviderReadV1<GenerationDiagnosticJoinV1>,
    pub test_attribution: GenerationProviderReadV1<GenerationTestJoinV1>,
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
    #[error("the Git/capture watermark names another reference")]
    ReferenceMismatch,
    #[error("the Git/capture watermark names another source revision")]
    StaleSourceRevision,
    #[error("the Git/capture content watermark is stale")]
    StaleContentWatermark,
    #[error("the Git/capture evidence digest does not bind this typed result")]
    StaleGitEvidence,
    #[error("Git blame path and supplied content identity differ")]
    BlamePathMismatch,
    #[error("duplicate Git symbol line binding for {0}")]
    DuplicateSymbolBinding(SymbolOccurrenceId),
    #[error("duplicate impact evidence for {0}")]
    DuplicateImpact(SymbolOccurrenceId),
    #[error("Git symbol line binding {0} belongs to another generation")]
    StaleSymbolGeneration(SymbolOccurrenceId),
    #[error("Git symbol line binding {0} names another file")]
    StaleSymbolFile(SymbolOccurrenceId),
    #[error("Git symbol line binding {0} has stale content")]
    StaleSymbolContent(SymbolOccurrenceId),
    #[error("Git symbol line binding {0} has an invalid line range")]
    InvalidSymbolLineRange(SymbolOccurrenceId),
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

impl GenerationGitHistoryJoinV1 {
    pub fn join(
        generation: &CodeGenerationManifestV1,
        snapshot: &ValidatedCodeSnapshotV1,
        history: &GitHistoryV1,
        git_watermark: &GenerationGitReadWatermarkV1,
    ) -> Result<Self, GenerationGitJoinErrorV1> {
        validate_generation_snapshot(generation, snapshot)?;
        history
            .validate()
            .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
        validate_git_read_watermark(snapshot, git_watermark)?;
        if history.repository != snapshot.snapshot.repository {
            return Err(GenerationGitJoinErrorV1::RepositoryMismatch);
        }
        if git_watermark.recompute_history_digest(history)? != git_watermark.evidence_digest {
            return Err(GenerationGitJoinErrorV1::StaleGitEvidence);
        }

        let coverage = if history.coverage.is_complete() && !history.truncated {
            GenerationGitHistoryJoinCoverageV1::Complete
        } else {
            GenerationGitHistoryJoinCoverageV1::Partial {
                degradations: history.coverage.degradations.clone(),
                truncated: history.truncated,
            }
        };
        Ok(Self {
            generation_id: generation.generation_id.clone(),
            code_snapshot_digest: generation.snapshot_digest.clone(),
            code_content_identity: snapshot.snapshot.content_identity.clone(),
            git_watermark: git_watermark.clone(),
            history: history.clone(),
            coverage,
        })
    }
}

impl GenerationGitBlameJoinV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn join(
        generation: &CodeGenerationManifestV1,
        snapshot: &ValidatedCodeSnapshotV1,
        blame: &GitBlameV1,
        git_watermark: &GenerationGitReadWatermarkV1,
        file_content: &GitFileContentIdentityV1,
        symbol_bindings: &[GitSymbolLineBindingV1],
    ) -> Result<Self, GenerationGitJoinErrorV1> {
        validate_generation_snapshot(generation, snapshot)?;
        blame
            .validate()
            .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
        validate_git_read_watermark(snapshot, git_watermark)?;
        if blame.repository != snapshot.snapshot.repository {
            return Err(GenerationGitJoinErrorV1::RepositoryMismatch);
        }
        if git_watermark.recompute_blame_digest(blame)? != git_watermark.evidence_digest {
            return Err(GenerationGitJoinErrorV1::StaleGitEvidence);
        }
        if blame.path != file_content.path {
            return Err(GenerationGitJoinErrorV1::BlamePathMismatch);
        }

        let snapshot_file = snapshot
            .snapshot
            .files
            .iter()
            .find(|file| file.logical_path == blame.path)
            .ok_or_else(|| GenerationGitJoinErrorV1::MissingSnapshotFile(blame.path.clone()))?;
        if snapshot_file.content_digest != file_content.content_digest {
            return Err(GenerationGitJoinErrorV1::ContentMismatch(
                blame.path.clone(),
            ));
        }

        let mut seen_symbols = BTreeSet::new();
        for binding in symbol_bindings {
            binding
                .symbol_occurrence_id
                .validate()
                .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
            if !seen_symbols.insert(&binding.symbol_occurrence_id) {
                return Err(GenerationGitJoinErrorV1::DuplicateSymbolBinding(
                    binding.symbol_occurrence_id.clone(),
                ));
            }
            if binding.generation_id != generation.generation_id {
                return Err(GenerationGitJoinErrorV1::StaleSymbolGeneration(
                    binding.symbol_occurrence_id.clone(),
                ));
            }
            if binding.file_occurrence_id != snapshot_file.file_occurrence_id {
                return Err(GenerationGitJoinErrorV1::StaleSymbolFile(
                    binding.symbol_occurrence_id.clone(),
                ));
            }
            if binding.content_digest != snapshot_file.content_digest {
                return Err(GenerationGitJoinErrorV1::StaleSymbolContent(
                    binding.symbol_occurrence_id.clone(),
                ));
            }
            if binding.start_line == 0
                || binding.end_line == 0
                || binding.start_line > binding.end_line
            {
                return Err(GenerationGitJoinErrorV1::InvalidSymbolLineRange(
                    binding.symbol_occurrence_id.clone(),
                ));
            }
        }

        let lines = blame
            .lines
            .iter()
            .map(|line| {
                let mut symbol_occurrence_ids: Vec<SymbolOccurrenceId> = symbol_bindings
                    .iter()
                    .filter(|binding| {
                        binding.start_line <= line.final_line && line.final_line <= binding.end_line
                    })
                    .map(|binding| binding.symbol_occurrence_id.clone())
                    .collect();
                symbol_occurrence_ids.sort();
                GenerationGitBlameLineJoinV1 {
                    line: line.clone(),
                    symbol_occurrence_ids,
                }
            })
            .collect();
        let coverage = if blame.availability != GitBlameAvailabilityV1::Available {
            GenerationGitBlameJoinCoverageV1::Unavailable {
                availability: blame.availability,
            }
        } else if blame.coverage.is_complete() {
            GenerationGitBlameJoinCoverageV1::Complete
        } else {
            GenerationGitBlameJoinCoverageV1::Partial {
                degradations: blame.coverage.degradations.clone(),
            }
        };

        Ok(Self {
            generation_id: generation.generation_id.clone(),
            code_snapshot_digest: generation.snapshot_digest.clone(),
            code_content_identity: snapshot.snapshot.content_identity.clone(),
            git_watermark: git_watermark.clone(),
            file_occurrence_id: snapshot_file.file_occurrence_id.clone(),
            content_digest: snapshot_file.content_digest.clone(),
            blame: blame.clone(),
            lines,
            coverage,
        })
    }
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
        Self::join_internal(
            generation,
            snapshot,
            diff,
            git_watermark,
            file_contents,
            None,
        )
    }

    /// Bind a typed Git diff and independently sourced graph, diagnostic, and
    /// test evidence to one immutable generation.
    pub fn join_with_context(
        generation: &CodeGenerationManifestV1,
        snapshot: &ValidatedCodeSnapshotV1,
        diff: &GitDiffV1,
        git_watermark: &GenerationGitWatermarkV1,
        file_contents: &[GitFileContentIdentityV1],
        context: &GenerationGitContextProvidersV1,
    ) -> Result<Self, GenerationGitJoinErrorV1> {
        Self::join_internal(
            generation,
            snapshot,
            diff,
            git_watermark,
            file_contents,
            Some(context),
        )
    }

    fn join_internal(
        generation: &CodeGenerationManifestV1,
        snapshot: &ValidatedCodeSnapshotV1,
        diff: &GitDiffV1,
        git_watermark: &GenerationGitWatermarkV1,
        file_contents: &[GitFileContentIdentityV1],
        context: Option<&GenerationGitContextProvidersV1>,
    ) -> Result<Self, GenerationGitJoinErrorV1> {
        validate_generation_snapshot(generation, snapshot)?;
        diff.validate()
            .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
        validate_git_watermark(snapshot, diff, git_watermark)?;

        let content_by_path = index_content_identity(file_contents)?;
        let snapshot_by_path: BTreeMap<&str, &SanitizedCodeFileV1> = snapshot
            .snapshot
            .files
            .iter()
            .map(|file| (file.logical_path.as_str(), file))
            .collect();

        let context_index = context
            .map(|context| validate_context(generation, snapshot, context))
            .transpose()?;
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
            let hunk_contexts = if matches!(join_state, GenerationGitFileJoinStateV1::Exact) {
                context_index
                    .as_ref()
                    .map(|index| {
                        git_file
                            .hunks
                            .iter()
                            .map(|hunk| {
                                hunk_context(generation, git_file, hunk, snapshot_file, index)
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
                    .unwrap_or_default()
            } else {
                Vec::new()
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
                hunk_contexts,
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
            diagnostics: context.map(|context| context.diagnostics.clone()),
            test_attribution: context.map(|context| context.test_attribution.clone()),
            coverage,
        })
    }
}

struct GenerationGitContextIndexV1<'a> {
    symbols_by_file: BTreeMap<&'a FileOccurrenceId, Vec<&'a GitSymbolLineBindingV1>>,
    impacts: BTreeMap<&'a SymbolOccurrenceId, &'a GenerationProviderReadV1<GenerationImpactJoinV1>>,
    diagnostics: Option<&'a GenerationDiagnosticJoinV1>,
    test_attribution: Option<&'a GenerationTestJoinV1>,
}

fn validate_context<'a>(
    generation: &CodeGenerationManifestV1,
    snapshot: &ValidatedCodeSnapshotV1,
    context: &'a GenerationGitContextProvidersV1,
) -> Result<GenerationGitContextIndexV1<'a>, GenerationGitJoinErrorV1> {
    context
        .diagnostics
        .validate()
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
    context
        .test_attribution
        .validate()
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
    let diagnostics = context.diagnostics.evidence.as_ref();
    let test_attribution = context.test_attribution.evidence.as_ref();
    for (generation_id, snapshot_digest, content_identity) in diagnostics
        .map(|join| {
            (
                &join.generation_id,
                &join.code_snapshot_digest,
                &join.code_content_identity,
            )
        })
        .into_iter()
        .chain(test_attribution.map(|join| {
            (
                &join.generation_id,
                &join.code_snapshot_digest,
                &join.code_content_identity,
            )
        }))
    {
        if generation_id != &generation.generation_id
            || snapshot_digest != &generation.snapshot_digest
            || content_identity != &snapshot.snapshot.content_identity
        {
            return Err(GenerationGitJoinErrorV1::StaleGenerationWatermark);
        }
    }

    let content_by_file: BTreeMap<&FileOccurrenceId, &ContentDigest> = snapshot
        .snapshot
        .files
        .iter()
        .map(|file| (&file.file_occurrence_id, &file.content_digest))
        .collect();
    let mut symbols_by_file: BTreeMap<&FileOccurrenceId, Vec<&GitSymbolLineBindingV1>> =
        BTreeMap::new();
    let mut seen_symbols = BTreeSet::new();
    for binding in &context.symbol_bindings {
        if !seen_symbols.insert(&binding.symbol_occurrence_id) {
            return Err(GenerationGitJoinErrorV1::DuplicateSymbolBinding(
                binding.symbol_occurrence_id.clone(),
            ));
        }
        if binding.generation_id != generation.generation_id {
            return Err(GenerationGitJoinErrorV1::StaleSymbolGeneration(
                binding.symbol_occurrence_id.clone(),
            ));
        }
        let Some(content) = content_by_file.get(&binding.file_occurrence_id) else {
            return Err(GenerationGitJoinErrorV1::StaleSymbolFile(
                binding.symbol_occurrence_id.clone(),
            ));
        };
        if *content != &binding.content_digest {
            return Err(GenerationGitJoinErrorV1::StaleSymbolContent(
                binding.symbol_occurrence_id.clone(),
            ));
        }
        if binding.start_line == 0 || binding.end_line == 0 || binding.start_line > binding.end_line
        {
            return Err(GenerationGitJoinErrorV1::InvalidSymbolLineRange(
                binding.symbol_occurrence_id.clone(),
            ));
        }
        symbols_by_file
            .entry(&binding.file_occurrence_id)
            .or_default()
            .push(binding);
    }
    for bindings in symbols_by_file.values_mut() {
        bindings.sort_by(|left, right| {
            left.start_line
                .cmp(&right.start_line)
                .then(left.end_line.cmp(&right.end_line))
                .then(left.symbol_occurrence_id.cmp(&right.symbol_occurrence_id))
        });
    }

    let mut impacts = BTreeMap::new();
    for (symbol, read) in &context.impacts {
        read.validate()
            .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
        if impacts.insert(symbol, read).is_some() {
            return Err(GenerationGitJoinErrorV1::DuplicateImpact(symbol.clone()));
        }
        if let Some(impact) = &read.evidence
            && (impact.generation_id != generation.generation_id
                || impact.code_snapshot_digest != generation.snapshot_digest
                || impact.code_content_identity != snapshot.snapshot.content_identity)
        {
            return Err(GenerationGitJoinErrorV1::StaleGenerationWatermark);
        }
    }
    Ok(GenerationGitContextIndexV1 {
        symbols_by_file,
        impacts,
        diagnostics,
        test_attribution,
    })
}

fn hunk_context(
    generation: &CodeGenerationManifestV1,
    git_file: &tracedecay_domain::GitFileDiffV1,
    hunk: &GitHunkV1,
    snapshot_file: &SanitizedCodeFileV1,
    context: &GenerationGitContextIndexV1<'_>,
) -> Result<GenerationGitHunkContextV1, GenerationGitJoinErrorV1> {
    let (start, lines) = if git_file.change == GitChangeKindV1::Deleted || hunk.new_lines == 0 {
        (hunk.old_start, hunk.old_lines)
    } else {
        (hunk.new_start, hunk.new_lines)
    };
    let end = start.saturating_add(lines.saturating_sub(1));
    let symbol_occurrence_ids = context
        .symbols_by_file
        .get(&snapshot_file.file_occurrence_id)
        .into_iter()
        .flatten()
        .filter(|binding| lines > 0 && binding.start_line <= end && binding.end_line >= start)
        .map(|binding| binding.symbol_occurrence_id.clone())
        .collect::<Vec<_>>();

    let impacts = symbol_occurrence_ids
        .iter()
        .filter_map(|symbol| context.impacts.get(symbol).copied().cloned())
        .collect::<Vec<_>>();
    let symbol_set = symbol_occurrence_ids.iter().collect::<BTreeSet<_>>();
    let mut diagnostic_anchors = context
        .diagnostics
        .into_iter()
        .flat_map(|diagnostics| &diagnostics.records)
        .filter_map(|record| match &record.disposition {
            GenerationDiagnosticDispositionV1::Current { attachment }
                if attachment.generation_id == generation.generation_id
                    && attachment.file_occurrence_id == snapshot_file.file_occurrence_id
                    && attachment
                        .symbol_occurrence_id
                        .as_ref()
                        .is_some_and(|symbol| symbol_set.contains(symbol)) =>
            {
                Some(attachment.diagnostic_anchor.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    diagnostic_anchors.sort();
    diagnostic_anchors.dedup();

    let mut hazard_anchors = Vec::new();
    let mut affected_tests = Vec::new();
    for read in &impacts {
        if let Some(impact) = &read.evidence {
            if let Some(graph) = &impact.graph_provider.evidence {
                hazard_anchors.extend(graph.evidence_anchors.iter().cloned());
            }
            affected_tests.extend(
                impact
                    .affected_tests
                    .iter()
                    .map(|binding| binding.symbol_occurrence_id.clone()),
            );
        }
    }
    for record in context
        .test_attribution
        .into_iter()
        .flat_map(|test_attribution| &test_attribution.records)
    {
        if matches!(
            record.disposition,
            GenerationTestJoinDispositionV1::Current { .. }
        ) && record
            .attribution
            .covered_occurrences
            .iter()
            .any(|covered| symbol_set.contains(covered))
        {
            affected_tests.push(record.attribution.test_occurrence.clone());
        }
    }
    hazard_anchors.sort();
    hazard_anchors.dedup();
    affected_tests.sort();
    affected_tests.dedup();
    Ok(GenerationGitHunkContextV1 {
        patch_digest: hunk.patch_digest.clone(),
        symbol_occurrence_ids,
        impacts,
        diagnostic_anchors,
        hazard_anchors,
        affected_tests,
    })
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
    if watermark.scope.worktree != snapshot.snapshot.worktree {
        return Err(GenerationGitJoinErrorV1::WorktreeMismatch);
    }
    if watermark.scope.reference != snapshot.snapshot.reference {
        return Err(GenerationGitJoinErrorV1::ReferenceMismatch);
    }
    if watermark.source_revision != snapshot.snapshot.source_revision {
        return Err(GenerationGitJoinErrorV1::StaleSourceRevision);
    }
    if watermark.snapshot_content_identity != snapshot.snapshot.content_identity {
        return Err(GenerationGitJoinErrorV1::StaleContentWatermark);
    }
    if watermark.diff_scope != diff.scope
        || watermark.recompute_evidence_digest(diff)? != watermark.git_snapshot_digest
    {
        return Err(GenerationGitJoinErrorV1::StaleGitEvidence);
    }
    Ok(())
}

fn validate_git_read_watermark(
    snapshot: &ValidatedCodeSnapshotV1,
    watermark: &GenerationGitReadWatermarkV1,
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
        .evidence_digest
        .validate()
        .map_err(|error| GenerationGitJoinErrorV1::Contract(error.to_string()))?;
    watermark.scope.validate()?;
    if watermark.repository != snapshot.snapshot.repository {
        return Err(GenerationGitJoinErrorV1::RepositoryMismatch);
    }
    if watermark.scope.worktree != snapshot.snapshot.worktree {
        return Err(GenerationGitJoinErrorV1::WorktreeMismatch);
    }
    if watermark.scope.reference != snapshot.snapshot.reference {
        return Err(GenerationGitJoinErrorV1::ReferenceMismatch);
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
