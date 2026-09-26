//! Canonical CLI/MCP wire contracts for the git-context reads the project's
//! graph-tool owner answers: affected tests, diff, commit, and PR context,
//! changelogs, and exact local-branch listing, search, and diffs.
//!
//! Presentation-only transport keys such as `format` are removed before these
//! request bodies are decoded. A git or branch refusal that answers the
//! request, rather than failing it, is a typed outcome variant the surfaces
//! render as a semantic tool error.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::RankedCandidate;

use super::{PrimitiveUnavailableEvidenceV1, RankedAffectedTestV1};

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedSurfaceRequestV1 {
    /// List of changed file paths to analyze.
    pub files: Vec<String>,
    /// Maximum dependency traversal depth (default: 5, at most 10).
    pub depth: Option<u32>,
    /// Custom glob pattern for test files (default: common test patterns).
    pub filter: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiffContextSurfaceRequestV1 {
    /// List of changed file paths.
    pub files: Vec<String>,
    /// Maximum impact traversal depth (default: 2, at most 10).
    pub depth: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChangelogSurfaceRequestV1 {
    /// Starting git ref (commit, branch, tag).
    pub from_ref: String,
    /// Ending git ref (commit, branch, tag).
    pub to_ref: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitContextSurfaceRequestV1 {
    /// If true, only analyze staged changes (default: false = all uncommitted
    /// changes).
    pub staged_only: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextSurfaceRequestV1 {
    /// Base branch or ref to compare against (default: detected repository
    /// default branch). A short branch name selects the descendant of its
    /// local and origin tracking tips; use an explicit ref when they diverge.
    pub base_ref: Option<String>,
    /// Head branch or ref (default: 'HEAD'). Accepts local branches,
    /// remote-tracking refs such as origin/topic, full refs, and Git revision
    /// expressions.
    pub head_ref: Option<String>,
    /// Maximum symbols returned on this page (default: 200, clamped to 1-500).
    pub maximum_symbols: Option<u32>,
    /// Authenticated continuation cursor returned by a previous page.
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchSearchSurfaceRequestV1 {
    /// Exact local branch name to search.
    pub branch: String,
    /// Search query string to match against symbol names.
    pub query: String,
    /// Maximum number of results to return (default: 10, at most 500).
    pub limit: Option<u32>,
    /// Authenticated continuation cursor returned by the preceding exact
    /// branch-search page.
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchDiffSurfaceRequestV1 {
    /// Base local branch name (e.g. 'main').
    pub base: String,
    /// Head local branch name (e.g. 'feature/foo'). Defaults to the current
    /// branch.
    pub head: Option<String>,
    /// Optional file path filter, only show diffs for symbols in this file.
    pub file: Option<String>,
    /// Optional kind filter, only show diffs for this symbol kind (e.g.
    /// 'function', 'struct').
    pub kind: Option<String>,
    /// Maximum combined added, removed, and changed results (default: 100,
    /// at most 256).
    pub limit: Option<u32>,
    /// Authenticated continuation cursor returned by the preceding exact
    /// branch-diff page.
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchListSurfaceRequestV1 {
    /// Maximum local refs to return (default: 100, at most 128).
    pub limit: Option<u32>,
    /// Return the stable lexical page after this branch name.
    pub after: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitReadCompleteV1 {
    Complete,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitReadPartialV1 {
    Partial,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitReadUnavailableV1 {
    Unavailable,
}

/// Whether a bounded page holds every answer or stopped at its limit.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitPageStatusV1 {
    Complete,
    Partial,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitResultLimitV1 {
    ResultLimit,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitReferenceLimitV1 {
    ReferenceLimit,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitToolErrorKindV1 {
    Git,
}

/// The git step that refused the read.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitToolOperationV1 {
    Diff,
    Status,
    Log,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitToolErrorV1 {
    pub kind: GitToolErrorKindV1,
    pub operation: GitToolOperationV1,
    pub message: String,
}

/// Git itself refused the refs, worktree status, or history the tool reads.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitToolFailureV1 {
    pub error: GitToolErrorV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedRankingMetadataV1 {
    pub strategy: String,
    pub distance: String,
    pub recommended_proximity: Vec<String>,
    pub compatibility_field: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedResultV1 {
    pub changed_files: Vec<String>,
    pub affected_tests: Vec<String>,
    pub count: usize,
    pub ranked_tests: Vec<RankedAffectedTestV1>,
    /// Tests within two dependency hops of a changed file.
    pub recommended_tests: Vec<String>,
    pub ranking_metadata: AffectedRankingMetadataV1,
}

/// A verified-graph symbol a git-context read reports.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitContextSymbolV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiffContextResultV1 {
    pub changed_files: Vec<String>,
    pub modified_symbols: Vec<GitContextSymbolV1>,
    pub impacted_symbols_count: usize,
    pub impacted_symbols: Vec<GitContextSymbolV1>,
    /// False when the impact walk stopped at its depth or budget with callers
    /// still unexplored.
    pub impact_complete: bool,
    pub affected_tests: Vec<String>,
}

/// A symbol the exact base/head branch-generation comparison reports.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitComparedSymbolV1 {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub content_digest: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolChangesCompleteV1 {
    pub status: GitReadCompleteV1,
}

/// Exact base/head symbol comparison was unavailable; the git evidence beside
/// it is still the answer.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolChangesUnavailableV1 {
    pub status: GitReadUnavailableV1,
    pub reason: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChangelogCompleteV1 {
    pub status: GitReadCompleteV1,
    pub from_ref: String,
    pub to_ref: String,
    pub changed_file_count: usize,
    pub changed_files: Vec<String>,
    pub base_generation: String,
    pub head_generation: String,
    pub symbols_added: Vec<GitComparedSymbolV1>,
    pub symbols_removed: Vec<GitComparedSymbolV1>,
    pub symbols_modified: Vec<GitComparedSymbolV1>,
    pub symbol_changes_coverage: SymbolChangesCompleteV1,
}

/// The tree diff without symbol changes: the refs are not both exact local
/// branches with sealed generations.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ChangelogPartialV1 {
    pub status: GitReadPartialV1,
    pub from_ref: String,
    pub to_ref: String,
    pub changed_file_count: usize,
    pub changed_files: Vec<String>,
    pub symbols_added: Vec<GitComparedSymbolV1>,
    pub symbols_removed: Vec<GitComparedSymbolV1>,
    pub symbols_modified: Vec<GitComparedSymbolV1>,
    pub symbol_changes_coverage: SymbolChangesUnavailableV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ChangelogResultV1 {
    Complete(ChangelogCompleteV1),
    Partial(ChangelogPartialV1),
    GitFailure(GitToolFailureV1),
}

/// A changed file's semantic role.
#[derive(
    Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum GitFileRoleV1 {
    Config,
    Docs,
    Source,
    Test,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
pub enum ConfigSummaryKindV1 {
    #[serde(rename = "config_summary")]
    ConfigSummary,
}

/// One entry per changed config file instead of one per config key.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSummaryV1 {
    pub file: String,
    pub kind: ConfigSummaryKindV1,
    pub config_keys: usize,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitFileRoleV1 {
    pub file: String,
    pub role: GitFileRoleV1,
    pub symbols: usize,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitSymbolV1 {
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CommitSymbolEntryV1 {
    Symbol(CommitSymbolV1),
    ConfigSummary(ConfigSummaryV1),
}

/// The commit category the changed file roles suggest.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
pub enum CommitCategoryV1 {
    #[serde(rename = "feature/fix (source + tests)")]
    SourceAndTests,
    #[serde(rename = "feature/fix/refactor")]
    Source,
    #[serde(rename = "test")]
    Test,
    #[serde(rename = "chore/docs/config")]
    Chore,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommitContextSummaryV1 {
    pub changed_files: Vec<CommitFileRoleV1>,
    pub symbols_by_role: BTreeMap<GitFileRoleV1, Vec<CommitSymbolEntryV1>>,
    /// Absent when the worktree has no changes.
    pub suggested_category: Option<CommitCategoryV1>,
    pub recent_commits: Vec<String>,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum CommitContextResultV1 {
    Summary(CommitContextSummaryV1),
    GitFailure(GitToolFailureV1),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitCommitSubjectV1 {
    pub hash: String,
    pub subject: String,
}

#[derive(
    Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Serialize,
)]
#[serde(rename_all = "snake_case")]
pub enum GitFileChangeStatusV1 {
    Added,
    Deleted,
    Modified,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitFileChangeV1 {
    pub path: String,
    pub status: GitFileChangeStatusV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum PrSymbolEntryV1 {
    Symbol(GitContextSymbolV1),
    ConfigSummary(ConfigSummaryV1),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrSymbolSelectionV1 {
    Unavailable,
    StablePrefix,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrSymbolPageV1 {
    pub limit: usize,
    pub returned: usize,
    pub has_more: bool,
    pub complete: bool,
    pub selection: PrSymbolSelectionV1,
    pub continuation_available: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrAnalysisCoverageV1 {
    pub seed_symbols_analyzed: usize,
    pub symbols_returned: usize,
    pub symbols_complete: bool,
    pub impact_nodes_admitted: usize,
    pub impact_nodes_returned: usize,
    pub direct_call_edges_admitted: usize,
    pub impact_bytes_admitted: usize,
    pub impact_partial: bool,
    pub complete: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrCoverageSelectionV1 {
    Unavailable,
    DeterministicBoundedPrefix,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrSelectionCoverageV1 {
    pub complete: bool,
    pub selection: PrCoverageSelectionV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrSymbolChangesCompleteV1 {
    pub status: GitReadCompleteV1,
    pub base_generation: String,
    pub head_generation: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextCompleteV1 {
    pub status: GitReadCompleteV1,
    pub base: String,
    pub head: String,
    pub base_oid: String,
    pub head_oid: String,
    pub merge_base: String,
    pub graph_generation: String,
    pub commits: Vec<GitCommitSubjectV1>,
    pub files_changed: usize,
    pub changes: Vec<GitFileChangeV1>,
    pub symbols_added: usize,
    pub symbols_removed: usize,
    pub symbols_modified: usize,
    pub added: Vec<PrSymbolEntryV1>,
    pub removed: Vec<GitComparedSymbolV1>,
    pub modified: Vec<PrSymbolEntryV1>,
    pub symbol_changes_coverage: PrSymbolChangesCompleteV1,
    pub next_cursor: Option<String>,
    pub symbol_page: PrSymbolPageV1,
    pub analysis_coverage: PrAnalysisCoverageV1,
    pub test_files_changed: Vec<String>,
    pub affected_tests: Vec<String>,
    pub affected_tests_coverage: PrSelectionCoverageV1,
    pub impacted_modules: Vec<String>,
    pub impacted_modules_coverage: PrSelectionCoverageV1,
}

/// The git comparison while exact base/head symbol comparison is unavailable
/// or does not describe the verified graph's head generation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextSymbolsUnavailableV1 {
    pub status: GitReadPartialV1,
    pub message: String,
    pub base: String,
    pub head: String,
    pub base_oid: String,
    pub head_oid: String,
    pub merge_base: String,
    pub graph_generation: String,
    pub commits: Vec<GitCommitSubjectV1>,
    pub files_changed: usize,
    pub changes: Vec<GitFileChangeV1>,
    pub symbols_added: usize,
    pub symbols_removed: usize,
    pub symbols_modified: usize,
    pub added: Vec<PrSymbolEntryV1>,
    pub removed: Vec<GitComparedSymbolV1>,
    pub modified: Vec<PrSymbolEntryV1>,
    pub symbol_changes_coverage: SymbolChangesUnavailableV1,
    pub next_cursor: Option<String>,
}

/// The git comparison while the verified graph generation is still warming.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrContextGraphPendingV1 {
    pub status: GitReadPartialV1,
    pub message: String,
    pub base: String,
    pub head: String,
    pub base_oid: String,
    pub head_oid: String,
    pub merge_base: String,
    /// No verified generation was admitted.
    pub graph_generation: Option<String>,
    pub commits: Vec<GitCommitSubjectV1>,
    pub files_changed: usize,
    pub changes: Vec<GitFileChangeV1>,
    pub symbols_added: usize,
    pub symbols_modified: usize,
    pub added: Vec<PrSymbolEntryV1>,
    pub modified: Vec<PrSymbolEntryV1>,
    pub next_cursor: Option<String>,
    pub symbol_page: PrSymbolPageV1,
    pub analysis_coverage: PrAnalysisCoverageV1,
    pub test_files_changed: Vec<String>,
    pub affected_tests: Vec<String>,
    pub affected_tests_coverage: PrSelectionCoverageV1,
    pub impacted_modules: Vec<String>,
    pub impacted_modules_coverage: PrSelectionCoverageV1,
    pub verified_graph_evidence: PrimitiveUnavailableEvidenceV1,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum PrContextResultV1 {
    Complete(Box<PrContextCompleteV1>),
    SymbolsUnavailable(Box<PrContextSymbolsUnavailableV1>),
    GraphPending(Box<PrContextGraphPendingV1>),
    GitFailure(GitToolFailureV1),
}

/// A local branch read refused before any snapshot was taken.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchReadUnavailableV1 {
    pub status: GitReadUnavailableV1,
    pub reason: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchSnapshotEntryV1 {
    pub branch: String,
    pub source_revision: String,
    pub source_tree: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchListPageV1 {
    pub status: GitPageStatusV1,
    pub reason: Option<GitReferenceLimitV1>,
    pub snapshot_count: usize,
    pub examined: usize,
    pub limit: usize,
    pub next_after: Option<String>,
    pub snapshots: Vec<BranchSnapshotEntryV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum BranchListResultV1 {
    Page(BranchListPageV1),
    Unavailable(BranchReadUnavailableV1),
}

/// The named branch does not resolve to a local commit.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchReferenceUnavailableV1 {
    pub status: GitReadUnavailableV1,
    pub branch: String,
    pub reason: String,
    pub retryable: bool,
}

/// No sealed code-index generation answers for the branch's commit.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchSearchUnavailableV1 {
    pub status: GitReadUnavailableV1,
    pub branch: String,
    pub source_revision: String,
    pub code_generation: Option<String>,
    pub reason: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchSearchHitV1 {
    pub candidate: RankedCandidate,
    pub name: Option<String>,
    pub qualified_name: Option<String>,
    pub kind: Option<String>,
    pub path: Option<String>,
    pub branch: String,
    pub source_reference: String,
    pub source_revision: String,
    pub source_tree: String,
    pub code_generation: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchSearchPageV1 {
    pub status: GitPageStatusV1,
    pub reason: Option<GitResultLimitV1>,
    pub branch: String,
    pub source_reference: String,
    pub source_revision: String,
    pub source_tree: String,
    pub code_generation: String,
    pub next_cursor: Option<String>,
    pub results: Vec<BranchSearchHitV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum BranchSearchResultV1 {
    Page(BranchSearchPageV1),
    SearchUnavailable(BranchSearchUnavailableV1),
    ReferenceUnavailable(BranchReferenceUnavailableV1),
}

/// A symbol in one branch's sealed generation.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchSymbolV1 {
    pub symbol_identity: String,
    pub symbol_occurrence_id: String,
    pub file_identity: String,
    pub file_occurrence_id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub content_digest: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "change", rename_all = "snake_case", deny_unknown_fields)]
pub enum BranchSymbolChangeV1 {
    Added {
        symbol: BranchSymbolV1,
    },
    Removed {
        symbol: BranchSymbolV1,
    },
    Changed {
        base: Box<BranchSymbolV1>,
        head: Box<BranchSymbolV1>,
    },
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchDiffSummaryV1 {
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchDiffCompleteV1 {
    pub status: GitReadCompleteV1,
    pub base: String,
    pub head: String,
    pub base_revision: String,
    pub base_tree: String,
    pub head_revision: String,
    pub head_tree: String,
    pub base_generation: String,
    pub head_generation: String,
    pub total_changes: usize,
    pub summary: BranchDiffSummaryV1,
    pub changes: Vec<BranchSymbolChangeV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchDiffPartialV1 {
    pub status: GitReadPartialV1,
    pub reason: GitResultLimitV1,
    pub base: String,
    pub head: String,
    pub base_revision: String,
    pub base_tree: String,
    pub head_revision: String,
    pub head_tree: String,
    pub base_generation: String,
    pub head_generation: String,
    pub total_changes: usize,
    pub next_cursor: String,
    pub summary: BranchDiffSummaryV1,
    pub changes: Vec<BranchSymbolChangeV1>,
}

/// Either branch of `base..head` does not resolve to a local commit.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchDiffReferenceUnavailableV1 {
    pub status: GitReadUnavailableV1,
    /// The requested range, `base..head`.
    pub base_or_head: String,
    pub reason: String,
    pub retryable: bool,
}

/// No sealed code-index generations compare the two branches' commits.
#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BranchDiffUnavailableV1 {
    pub status: GitReadUnavailableV1,
    pub base: String,
    pub head: String,
    pub base_revision: String,
    pub head_revision: String,
    pub base_generation: Option<String>,
    pub head_generation: Option<String>,
    pub reason: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum BranchDiffResultV1 {
    Complete(BranchDiffCompleteV1),
    Partial(BranchDiffPartialV1),
    DiffUnavailable(BranchDiffUnavailableV1),
    ReferenceUnavailable(BranchDiffReferenceUnavailableV1),
}
