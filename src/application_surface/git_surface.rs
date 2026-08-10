//! Typed Git application-surface requests and bounded Git-read decoding.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracedecay_application::IdempotencyKey;
use tracedecay_domain::git::{GitDiffScopeV1, GitOidV1};
use tracedecay_domain::{
    GitIndexCommitIntentV1, GitIndexPreviewId, GitIndexTransactionOperationV1, ManifestDigest,
};

use super::{ApplicationSurfaceAdapterError, ApplicationSurfaceOperation};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitPreviewSurfaceRequest {
    pub operation: GitIndexTransactionOperationV1,
    #[serde(default)]
    pub preview_input_id: Option<GitIndexPreviewId>,
    #[serde(default)]
    pub selected_hunk_digests: Vec<ManifestDigest>,
    #[serde(default)]
    pub commit_intent: Option<GitIndexCommitIntentV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitApplySurfaceRequest {
    pub preview_id: GitIndexPreviewId,
    pub preview_digest: ManifestDigest,
    pub idempotency_key: IdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitReadSurfaceRequest {
    pub request: tracedecay_usecases::git_reads::GitReadRequestV1,
    pub max_entries: u32,
    pub max_bytes: u64,
}

pub(super) fn parse_git_read_surface_request(
    operation: ApplicationSurfaceOperation,
    value: Value,
) -> Result<GitReadSurfaceRequest, ApplicationSurfaceAdapterError> {
    let object = value
        .as_object()
        .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?;
    let bounded_u64 = |name: &str, default: u64, maximum: u64| match object.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|value| (1..=maximum).contains(value))
            .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
    };
    let boolean = |name: &str, default: bool| match object.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_bool()
            .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
    };
    let optional_string = |name: &str| match object.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_owned()))
            .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
    };
    let max_entries = bounded_u64(
        "max_entries",
        u64::from(crate::git_query::GIT_QUERY_DEFAULT_MAX_ENTRIES),
        u64::from(crate::git_query::GIT_QUERY_DEFAULT_MAX_ENTRIES),
    )? as u32;
    let max_bytes = bounded_u64(
        "max_bytes",
        crate::git_query::GIT_QUERY_DEFAULT_MAX_BYTES,
        crate::git_query::GIT_QUERY_DEFAULT_MAX_BYTES,
    )?;
    let string = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty() && value.trim() == *value)
            .map(str::to_owned)
            .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)
    };
    let scope_name = match object.get("scope") {
        None => "working_tree",
        Some(value) => value
            .as_str()
            .ok_or(ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?,
    };
    let scope = |allow_commit_range: bool| match scope_name {
        "working_tree" if !object.contains_key("base") && !object.contains_key("head") => {
            Ok(GitDiffScopeV1::WorkingTree)
        }
        "staged" if !object.contains_key("base") && !object.contains_key("head") => {
            Ok(GitDiffScopeV1::Staged)
        }
        "commit_range" if allow_commit_range => Ok(GitDiffScopeV1::CommitRange {
            base: GitOidV1::new(string("base")?)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?,
            head: GitOidV1::new(string("head")?)
                .map_err(|_| ApplicationSurfaceAdapterError::InvalidSurfaceRequest)?,
        }),
        _ => Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
    };
    let request = match operation {
        ApplicationSurfaceOperation::GitStatus => {
            tracedecay_usecases::git_reads::GitReadRequestV1::Status
        }
        ApplicationSurfaceOperation::GitDiff => {
            tracedecay_usecases::git_reads::GitReadRequestV1::Diff {
                scope: scope(true)?,
            }
        }
        ApplicationSurfaceOperation::GitHistory => {
            tracedecay_usecases::git_reads::GitReadRequestV1::History {
                max_count: bounded_u64("count", 100, 1_000)? as u32,
                path: optional_string("path")?,
                follow: boolean("follow", false)?,
                first_parent: boolean("first_parent", false)?,
            }
        }
        ApplicationSurfaceOperation::GitBlame => {
            tracedecay_usecases::git_reads::GitReadRequestV1::Blame {
                path: string("path")?,
                follow_renames: boolean("follow_renames", false)?,
            }
        }
        ApplicationSurfaceOperation::GitHunks => {
            tracedecay_usecases::git_reads::GitReadRequestV1::Hunks {
                scope: scope(false)?,
                daemon_binding: None,
            }
        }
        _ => return Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest),
    };
    let allowed = match operation {
        ApplicationSurfaceOperation::GitStatus => &["max_entries", "max_bytes"][..],
        ApplicationSurfaceOperation::GitDiff => {
            &["scope", "base", "head", "max_entries", "max_bytes"][..]
        }
        ApplicationSurfaceOperation::GitHistory => &[
            "count",
            "path",
            "follow",
            "first_parent",
            "max_entries",
            "max_bytes",
        ][..],
        ApplicationSurfaceOperation::GitBlame => {
            &["path", "follow_renames", "max_entries", "max_bytes"][..]
        }
        ApplicationSurfaceOperation::GitHunks => &["scope", "max_entries", "max_bytes"][..],
        _ => &[],
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(ApplicationSurfaceAdapterError::InvalidSurfaceRequest);
    }
    Ok(GitReadSurfaceRequest {
        request,
        max_entries,
        max_bytes,
    })
}
