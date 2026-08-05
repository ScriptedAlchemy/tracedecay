//! Normalized ingress contracts for the mounted Git application operations.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_application::IdempotencyKey;
use tracedecay_domain::git::{
    GitIndexCommitIntentV1, GitIndexPreviewId, GitIndexPreviewV1, GitIndexTransactionOperationV1,
    HunkRefV1, RepositoryStateSnapshotV1,
};

use super::GitReadRequestV1;

/// Normalized preview request admitted by the Git application handler.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitPreviewSurfaceRequest {
    pub operation: GitIndexTransactionOperationV1,
    /// Compatibility input only. The daemon always replaces this value with a
    /// freshly minted preview identity before application admission.
    #[serde(default)]
    pub preview_id: GitIndexPreviewId,
    pub repository_snapshot: RepositoryStateSnapshotV1,
    #[serde(default)]
    pub selected_hunks: Vec<HunkRefV1>,
    #[serde(default)]
    pub commit_intent: Option<GitIndexCommitIntentV1>,
}

/// Normalized apply request admitted by the Git application handler.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitApplySurfaceRequest {
    pub preview: GitIndexPreviewV1,
    pub idempotency_key: IdempotencyKey,
}

/// Normalized bounded read request admitted by the Git application handler.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitReadSurfaceRequest {
    pub request: GitReadRequestV1,
    pub max_entries: u32,
    pub max_bytes: u64,
}
