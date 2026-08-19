//! Transport-neutral read-only Git intelligence contracts.

use std::path::Path;

use serde::Serialize;
use thiserror::Error;
use tracedecay_domain::{
    DomainError, GitBlameV1, GitDiffScopeV1, GitDiffV1, GitHistoryV1, GitOidV1, GitStatusV1,
    HunkRefV1, ManifestDigest, RepositoryId, WorktreeId,
};

/// Upper bound for bounded history requests.
pub const GIT_HISTORY_MAX_COUNT_LIMIT: u32 = 1_000;

/// Hard ceiling for one historical blob materialized by a Git adapter.
pub const GIT_HISTORICAL_BLOB_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Errors from a read-only Git intelligence adapter.
#[derive(Debug, Error)]
pub enum GitIntelligenceError {
    #[error("git executable unavailable: {0}")]
    GitUnavailable(String),
    #[error("git read cancelled")]
    Cancelled,
    #[error("git read deadline exceeded")]
    DeadlineExceeded,
    #[error("git {stream} output exceeded {bound} bytes")]
    OutputLimitExceeded { stream: &'static str, bound: usize },
    #[error("not a git repository: {0}")]
    NotARepository(String),
    #[error("git {operation} failed ({status}): {stderr}")]
    GitFailed {
        operation: &'static str,
        status: String,
        stderr: String,
    },
    #[error("git {operation} produced malformed output: {detail}")]
    MalformedOutput {
        operation: &'static str,
        detail: String,
    },
    #[error("read-only adapter refused git {0}: not an admitted read operation")]
    ReadOnlyViolation(String),
    #[error("HunkRef cannot be minted for a commit-range diff")]
    HunkRefNotMintable,
    #[error("cannot mint HunkRef for {path}: {reason}")]
    UnmintableHunkKind { path: String, reason: &'static str },
    #[error("invalid historical repository-relative path: {0}")]
    InvalidHistoricalPath(String),
    #[error("historical blob exceeds byte bound: {actual} bytes > bound {bound}")]
    HistoricalBlobBoundExceeded { bound: u64, actual: u64 },
    #[error("domain validation failed: {0}")]
    Domain(#[from] DomainError),
}

/// Bounded history request profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHistoryRequest {
    pub max_count: u32,
    pub path: Option<String>,
    pub follow: bool,
    pub first_parent: bool,
}

impl Default for GitHistoryRequest {
    fn default() -> Self {
        Self {
            max_count: 100,
            path: None,
            follow: false,
            first_parent: false,
        }
    }
}

/// Blame request profile for one path at the current HEAD/worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitBlameRequest {
    pub path: String,
    pub follow_renames: bool,
}

/// One exact, bounded historical blob read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHistoricalBlobRequestV1 {
    pub commit: GitOidV1,
    pub path: String,
    pub max_bytes: u64,
    pub include_bytes: bool,
}

/// Historical blob content, or an explicit absent-path result.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GitHistoricalBlobV1 {
    pub repository: RepositoryId,
    pub worktree: WorktreeId,
    pub commit: GitOidV1,
    pub path: String,
    pub blob_oid: Option<GitOidV1>,
    pub bytes: Option<Vec<u8>>,
}

/// Narrow read port used by code-index historical reconstruction.
pub trait GitHistoricalBlobReadPort {
    fn historical_blob(
        &self,
        request: &GitHistoricalBlobRequestV1,
    ) -> Result<GitHistoricalBlobV1, GitIntelligenceError>;
}

/// Full read-only Git intelligence port.
pub trait GitReadPort: GitHistoricalBlobReadPort {
    fn status(&self) -> Result<GitStatusV1, GitIntelligenceError>;

    fn diff(&self, scope: &GitDiffScopeV1) -> Result<GitDiffV1, GitIntelligenceError>;

    fn history(&self, request: &GitHistoryRequest) -> Result<GitHistoryV1, GitIntelligenceError>;

    fn blame(&self, request: &GitBlameRequest) -> Result<GitBlameV1, GitIntelligenceError>;

    fn hunk_refs(
        &self,
        scope: &GitDiffScopeV1,
        preview_id: &str,
        snapshot_digest: &ManifestDigest,
    ) -> Result<Vec<HunkRefV1>, GitIntelligenceError>;
}

/// Whether a path is one canonical repository-relative path.
pub fn is_canonical_repository_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !Path::new(path).is_absolute()
        && path
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}
