//! Shared provenance for prospective file context and exact source comparisons.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{GitChangeKindV1, GitCoverageV1, GitOidV1};

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GitContextComparisonV1 {
    /// Caller-selected files: symbols may be unchanged or not yet edited.
    ProposedFiles,
    CommitRange {
        base: GitOidV1,
        head: GitOidV1,
    },
    /// Pinned HEAD (or empty tree) to the selected immutable index blobs.
    Staged {
        head: Option<GitOidV1>,
    },
    /// Selected index blobs to captured working-tree bytes.
    WorkingTree,
    /// Net change from pinned HEAD (or empty tree), including untracked files.
    HeadToWorktree {
        head: Option<GitOidV1>,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitContextSourceSideV1 {
    Before,
    After,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitDeclarationChangeKindV1 {
    Added,
    Removed,
    Modified,
    SignatureChanged,
    VisibilityChanged,
    Renamed,
    AmbiguousOverload,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitContextFileComparisonV1 {
    pub change: GitChangeKindV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_blob: Option<GitOidV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_blob: Option<GitOidV1>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitContextSourceGapV1 {
    pub file: String,
    pub reason: String,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GitContextSourceConsistencyV1 {
    ImmutableBlobs,
    /// Each file is checked around its bounded read; this is not an atomic
    /// snapshot of the entire working tree.
    PerFileWorktreeCapture,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GitContextSourceCoverageV1 {
    pub complete: bool,
    pub git: GitCoverageV1,
    pub gaps: Vec<GitContextSourceGapV1>,
    pub consistency: GitContextSourceConsistencyV1,
}
