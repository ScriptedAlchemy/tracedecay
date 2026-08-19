//! Canonical public Git wire contracts.
//!
//! These types are shared by catalog schema generation and root transport
//! parsing, so an SDK schema cannot drift from the request the daemon admits
//! or from the typed result it returns.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    GitBlameV1, GitCoverageV1, GitDiffV1, GitHeadStateV1, GitHistoryV1, GitIndexCommitIntentV1,
    GitIndexPreviewId, GitIndexTransactionOperationV1, GitOidV1, GitOperationStateV1, HunkRefV1,
    ManifestDigest, RepositoryId, UtcMicros,
};

use crate::IdempotencyKey;

/// Public MCP/CLI request for one daemon-owned Git index preview.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitPreviewSurfaceRequest {
    pub operation: GitIndexTransactionOperationV1,
    /// Hunk preview input minted by `git_hunks`; required for stage/unstage.
    #[serde(default)]
    pub preview_input_id: Option<GitIndexPreviewId>,
    /// Selection digests drawn from the referenced preview input.
    #[serde(default)]
    pub selected_hunk_digests: Vec<ManifestDigest>,
    #[serde(default)]
    pub commit_intent: Option<GitIndexCommitIntentV1>,
}

/// Public MCP/CLI request to apply one immutable Git index preview.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitApplySurfaceRequest {
    pub preview_id: GitIndexPreviewId,
    pub preview_digest: ManifestDigest,
    pub idempotency_key: IdempotencyKey,
}

/// Request shape for the public `git_status` surface.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitStatusSurfaceRequest {
    pub max_entries: Option<u32>,
    pub max_bytes: Option<u64>,
}

/// Flat public selector for an admitted Git diff scope.
///
/// This intentionally differs from [`tracedecay_domain::GitDiffScopeV1`]: MCP
/// and CLI accept `scope`, `base`, and `head` as sibling fields, then
/// transport parsing builds the canonical domain scope after checking their
/// legal combinations.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitSurfaceDiffScopeV1 {
    #[default]
    WorkingTree,
    Staged,
    CommitRange,
}

/// Request shape for the public `git_diff` surface.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitDiffSurfaceRequest {
    #[serde(default)]
    pub scope: GitSurfaceDiffScopeV1,
    pub base: Option<GitOidV1>,
    pub head: Option<GitOidV1>,
    pub max_entries: Option<u32>,
    pub max_bytes: Option<u64>,
}

/// Request shape for the public `git_history` surface.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHistorySurfaceRequest {
    pub count: Option<u32>,
    pub path: Option<String>,
    #[serde(default)]
    pub follow: bool,
    #[serde(default)]
    pub first_parent: bool,
    pub max_entries: Option<u32>,
    pub max_bytes: Option<u64>,
}

/// Request shape for the public `git_blame` surface.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitBlameSurfaceRequest {
    pub path: String,
    #[serde(default)]
    pub follow_renames: bool,
    pub max_entries: Option<u32>,
    pub max_bytes: Option<u64>,
}

/// Request shape for the public `git_hunks` surface.
///
/// The daemon captures exact repository state itself and injects the private
/// preview binding after capture, so the public wire carries only the diff
/// scope (commit ranges cannot mint applicable hunks).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHunksSurfaceRequest {
    #[serde(default)]
    pub scope: GitSurfaceDiffScopeV1,
    pub max_entries: Option<u32>,
    pub max_bytes: Option<u64>,
}

/// One typed query result with its merged coverage. `coverage` is the
/// adapter-reported coverage plus any query-level degradation (entry-bound
/// truncation); `truncated_by_bound` distinguishes query-level truncation
/// from adapter-level capture bounds.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitQueryEnvelopeV1<T> {
    pub value: T,
    pub coverage: GitCoverageV1,
    pub truncated_by_bound: bool,
}

/// Bounded status summary derived from the typed
/// [`tracedecay_domain::git::GitStatusV1`]: HEAD and operation state,
/// per-class counts, and a bounded sorted sample of changed paths.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitStatusSummaryV1 {
    pub repository: RepositoryId,
    pub head: GitHeadStateV1,
    pub operation: GitOperationStateV1,
    pub staged: u32,
    pub unstaged: u32,
    pub conflicted: u32,
    pub untracked: u32,
    pub ignored: u32,
    /// Sorted, de-duplicated changed paths, truncated at the query entry bound.
    pub changed_paths: Vec<String>,
    pub schema_version: String,
}

/// One minted hunk selection: the canonical selection digest plus the
/// `HunkRef` identity evidence it selects.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHunkPreviewEntryV1 {
    pub digest: ManifestDigest,
    pub hunk: HunkRefV1,
}

/// Bounded, expiring preview input minted by `git_hunks` from one exact
/// daemon-captured repository snapshot.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHunkPreviewInputV1 {
    pub preview_input_id: GitIndexPreviewId,
    pub repository_snapshot_digest: ManifestDigest,
    pub expires_at: UtcMicros,
    pub hunks: Vec<GitHunkPreviewEntryV1>,
}

/// Actual payload emitted by each public Git read operation.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "query", content = "result", rename_all = "snake_case")]
pub enum GitReadResultV1 {
    Status(GitQueryEnvelopeV1<GitStatusSummaryV1>),
    Diff(GitQueryEnvelopeV1<GitDiffV1>),
    History(GitQueryEnvelopeV1<GitHistoryV1>),
    Blame(GitQueryEnvelopeV1<GitBlameV1>),
    Hunks(GitQueryEnvelopeV1<GitHunkPreviewInputV1>),
}
