//! Typed branch-scoped code-query contracts.
//!
//! Branch selectors remain native ref labels at the transport edge. The daemon
//! resolves them against one exact registered project/repository/worktree
//! authority before any graph read. Paths and graph-store locations never
//! cross this boundary.

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::{
    CanonicalGitRefNameV1, ConfigurationRevisionId, GitOidV1, ManifestDigest, ProjectId, RefId,
    RepositoryId, UtcMicros, WorktreeId,
};

use crate::{ApplicationContractError, CancellationSignal, Deadline};

pub const BRANCH_SEARCH_CAPABILITY_ID_V1: &str = "capability.application.branch.search";
pub const BRANCH_DIFF_CAPABILITY_ID_V1: &str = "capability.application.branch.diff";

pub const BRANCH_QUERY_MAX_LIMIT_V1: u32 = 500;
pub const BRANCH_QUERY_DEFAULT_LIMIT_V1: u32 = 10;
const BRANCH_NAME_MAX_BYTES_V1: usize = 1_024;
const BRANCH_QUERY_MAX_BYTES_V1: usize = 8 * 1_024;
const BRANCH_QUERY_MAX_CURSOR_BYTES_V1: usize = 128 * 1_024;

const fn default_branch_query_limit() -> u32 {
    BRANCH_QUERY_DEFAULT_LIMIT_V1
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchSearchRequestV1 {
    pub branch: String,
    pub query: String,
    #[serde(default = "default_branch_query_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl BranchSearchRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        validate_branch_name(&self.branch)?;
        if self.query.trim().is_empty()
            || self.query.len() > BRANCH_QUERY_MAX_BYTES_V1
            || self.query.chars().any(char::is_control)
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "branch search query",
            });
        }
        if self.limit == 0 || self.limit > BRANCH_QUERY_MAX_LIMIT_V1 {
            return Err(ApplicationContractError::InvalidRange {
                field: "branch search limit",
            });
        }
        validate_cursor(self.cursor.as_deref())?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchDiffRequestV1 {
    pub base: Option<String>,
    pub head: Option<String>,
    pub file: Option<String>,
    pub kind: Option<String>,
    #[serde(default = "default_branch_query_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl BranchDiffRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if let Some(base) = &self.base {
            validate_branch_name(base)?;
        }
        if let Some(head) = &self.head {
            validate_branch_name(head)?;
        }
        if self
            .file
            .as_deref()
            .is_some_and(|file| !is_canonical_optional_filter(file))
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "branch diff file filter",
            });
        }
        if self
            .kind
            .as_deref()
            .is_some_and(|kind| !is_canonical_optional_filter(kind))
        {
            return Err(ApplicationContractError::InvalidRange {
                field: "branch diff kind filter",
            });
        }
        if self.limit == 0 || self.limit > BRANCH_QUERY_MAX_LIMIT_V1 {
            return Err(ApplicationContractError::InvalidRange {
                field: "branch diff limit",
            });
        }
        validate_cursor(self.cursor.as_deref())?;
        Ok(())
    }
}

fn validate_cursor(cursor: Option<&str>) -> Result<(), ApplicationContractError> {
    if cursor.is_some_and(|cursor| {
        cursor.is_empty()
            || cursor.len() > BRANCH_QUERY_MAX_CURSOR_BYTES_V1
            || cursor.chars().any(char::is_whitespace)
    }) {
        return Err(ApplicationContractError::InvalidRange {
            field: "branch query cursor",
        });
    }
    Ok(())
}

fn validate_branch_name(branch: &str) -> Result<(), ApplicationContractError> {
    if branch.is_empty()
        || branch.len() > BRANCH_NAME_MAX_BYTES_V1
        || branch.starts_with("refs/")
        || CanonicalGitRefNameV1::new(format!("refs/heads/{branch}")).is_err()
    {
        return Err(ApplicationContractError::InvalidRange {
            field: "branch name",
        });
    }
    Ok(())
}

fn is_canonical_optional_filter(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= BRANCH_QUERY_MAX_BYTES_V1
        && !value.starts_with('/')
        && !value.contains('\0')
        && !value.chars().any(char::is_control)
        && value
            .split('/')
            .all(|component| !matches!(component, "" | "." | ".."))
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub enum BranchQueryRequestV1 {
    Search(BranchSearchRequestV1),
    Diff(BranchDiffRequestV1),
}

impl BranchQueryRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        match self {
            Self::Search(request) => request.validate(),
            Self::Diff(request) => request.validate(),
        }
    }
}

/// Live request controls retained from daemon admission.
#[derive(Clone, Debug, Default)]
pub struct BranchQueryControlsV1 {
    pub deadline: Option<Deadline>,
    pub cancellation: Option<CancellationSignal>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchGraphGenerationV1 {
    pub graph_scope_id: String,
    pub source_oid: GitOidV1,
    pub content_digest: ManifestDigest,
    pub recorded_sync_at: Option<UtcMicros>,
    pub generation_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchAuthorizationEpochV1 {
    pub configuration_revision: ConfigurationRevisionId,
    pub configuration_digest: ManifestDigest,
    pub configuration_provenance_digest: ManifestDigest,
    pub access_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchSnapshotIdentityV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub worktree_id: WorktreeId,
    pub reference: RefId,
    pub scope_digest: ManifestDigest,
    pub authorization: BranchAuthorizationEpochV1,
    pub generation: BranchGraphGenerationV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BranchSearchMatchV1 {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub signature: Option<String>,
    pub score: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BranchSearchResultV1 {
    pub snapshot: BranchSnapshotIdentityV1,
    pub total: u64,
    pub items: Vec<BranchSearchMatchV1>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchDiffSymbolV1 {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub signature: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchChangedSymbolV1 {
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file: String,
    pub line: u32,
    pub base_signature: Option<String>,
    pub head_signature: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchDiffSummaryV1 {
    pub added: u64,
    pub removed: u64,
    pub changed: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BranchDiffResultV1 {
    pub base: BranchSnapshotIdentityV1,
    pub head: BranchSnapshotIdentityV1,
    pub note: Option<String>,
    pub summary: BranchDiffSummaryV1,
    pub added: Vec<BranchDiffSymbolV1>,
    pub removed: Vec<BranchDiffSymbolV1>,
    pub changed: Vec<BranchChangedSymbolV1>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
pub enum BranchQueryResultV1 {
    Search(BranchSearchResultV1),
    Diff(BranchDiffResultV1),
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BranchQueryPartialReasonV1 {
    ResultLimitReached,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BranchQueryUnavailableReasonV1 {
    InvalidRequest,
    RegistryUnavailable,
    ProjectUnavailable,
    BranchUnavailable,
    GraphAuthorityUnavailable,
    GenerationUnavailable,
    CursorUnavailable,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BranchQueryStaleReasonV1 {
    ReferenceMoved,
    GraphGenerationChanged,
    AuthorizationEpochChanged,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum BranchQueryOutcomeV1 {
    Complete {
        result: BranchQueryResultV1,
    },
    Partial {
        result: BranchQueryResultV1,
        reason: BranchQueryPartialReasonV1,
    },
    Denied,
    Cancelled,
    TimedOut,
    Stale {
        reason: BranchQueryStaleReasonV1,
    },
    Unavailable {
        reason: BranchQueryUnavailableReasonV1,
    },
}

pub type BranchQueryFuture<'a> = Pin<Box<dyn Future<Output = BranchQueryOutcomeV1> + Send + 'a>>;

/// Daemon-owned branch search/diff application boundary.
pub trait BranchQueryPort: Send + Sync {
    fn execute<'a>(
        &'a self,
        request: BranchQueryRequestV1,
        controls: BranchQueryControlsV1,
    ) -> BranchQueryFuture<'a>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_reject_empty_or_unbounded_inputs() {
        let empty = BranchSearchRequestV1 {
            branch: String::new(),
            query: "needle".to_owned(),
            limit: 10,
            cursor: None,
        };
        assert!(empty.validate().is_err());

        let unbounded = BranchSearchRequestV1 {
            branch: "main".to_owned(),
            query: "needle".to_owned(),
            limit: BRANCH_QUERY_MAX_LIMIT_V1 + 1,
            cursor: None,
        };
        assert!(unbounded.validate().is_err());

        let invalid_filter = BranchDiffRequestV1 {
            base: Some("main".to_owned()),
            head: Some("feature".to_owned()),
            file: Some("../secret".to_owned()),
            kind: None,
            limit: 10,
            cursor: None,
        };
        assert!(invalid_filter.validate().is_err());

        let transport_ref = BranchSearchRequestV1 {
            branch: "refs/heads/main".to_owned(),
            query: "needle".to_owned(),
            limit: 10,
            cursor: None,
        };
        assert!(transport_ref.validate().is_err());

        let control_query = BranchSearchRequestV1 {
            branch: "main".to_owned(),
            query: "needle\nsecond request".to_owned(),
            limit: 10,
            cursor: None,
        };
        assert!(control_query.validate().is_err());
    }

    #[test]
    fn wire_defaults_search_limit_without_a_transport_alias() {
        let request: BranchSearchRequestV1 = serde_json::from_value(serde_json::json!({
            "branch": "main",
            "query": "needle"
        }))
        .expect("request");
        assert_eq!(request.limit, BRANCH_QUERY_DEFAULT_LIMIT_V1);
    }
}
