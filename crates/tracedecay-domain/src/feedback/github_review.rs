//! Read-only GitHub review-ingress contracts.
//!
//! These values preserve observed review state and immutable anchors. They
//! contain no outbound operation, credential, HTTP-method, or client type:
//! callers can represent only allowlisted REST `GET` reads and GraphQL
//! `query` reads. A connector implementation therefore receives no typed
//! request that can express a GitHub mutation.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::code_intelligence::identity::{
    ContentDigest, FileOccurrenceId, SourceSpan, SymbolOccurrenceId,
};
use crate::research::{
    CommitId, DomainError, ManifestDigest, ProviderId, RepositoryId, RetrievalAnchorId, UtcMicros,
};

use super::FeedbackScopeV1;

crate::canonical_text::validated_string_newtype!(
    plain,
    DomainError,
    super::validate_label;
    GitHubPullRequestIdV1 => "github pull request id",
    GitHubReviewIdV1 => "github review id",
    GitHubReviewThreadIdV1 => "github review thread id",
    GitHubReviewCommentIdV1 => "github review comment id",
    GitHubReviewEtagV1 => "github review etag",
    GitHubReviewCursorV1 => "github review cursor",
);

/// Closed allowlist for the review-ingress connector. REST variants denote
/// exactly one HTTP `GET`; the GraphQL variant denotes a normalized `query`
/// document, never a mutation. There is intentionally no generic endpoint,
/// HTTP-method, or mutation variant.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReviewReadOperationV1 {
    RestGetPullRequest,
    RestListPullRequestReviews,
    RestListPullRequestReviewComments,
    GraphQlQueryPullRequestReviewThreads,
}

impl GitHubReviewReadOperationV1 {
    /// This is structurally true for every representable operation.
    pub const fn is_read_only(self) -> bool {
        true
    }

    pub const fn is_rest(self) -> bool {
        matches!(
            self,
            Self::RestGetPullRequest
                | Self::RestListPullRequestReviews
                | Self::RestListPullRequestReviewComments
        )
    }

    pub const fn is_graphql_query(self) -> bool {
        matches!(self, Self::GraphQlQueryPullRequestReviewThreads)
    }
}

/// Observed lifecycle of an item or thread. This remains independent from
/// [`GitHubReviewIngressProviderOutcomeV1`], which describes a fetch attempt.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReviewLifecycleV1 {
    Current,
    Outdated,
    Resolved,
    Edited,
    Deleted,
}

/// Outcome of a read-ingress fetch, refresh, or expansion attempt. It never
/// represents an outbound comment or thread action.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReviewIngressProviderOutcomeV1 {
    Complete,
    Partial,
    Unavailable,
    Denied,
    RateLimited,
    Stale,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReviewAuthorClassV1 {
    Bot,
    Maintainer,
    OtherObservedRole,
}

/// Review-level state reported by GitHub. This is observed framing only and
/// never upgrades finding severity, confidence, or coverage.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReviewStateV1 {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReviewCoverageV1 {
    Complete,
    Partial,
    Unavailable,
    Denied,
    Stale,
}

/// Opaque checkpoint from a completed or partial read. It captures only cache,
/// pagination, and rate-limit state; it cannot express a write precondition or
/// an outbound operation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewReadCheckpointV1 {
    pub etag: Option<GitHubReviewEtagV1>,
    pub next_cursor: Option<GitHubReviewCursorV1>,
    pub rate_limit: Option<GitHubReviewRateLimitCheckpointV1>,
}

impl GitHubReviewReadCheckpointV1 {
    pub fn validate_for(
        &self,
        outcome: GitHubReviewIngressProviderOutcomeV1,
    ) -> Result<(), DomainError> {
        self.etag
            .as_ref()
            .map_or(Ok(()), GitHubReviewEtagV1::validate)?;
        self.next_cursor
            .as_ref()
            .map_or(Ok(()), GitHubReviewCursorV1::validate)?;
        self.rate_limit
            .as_ref()
            .map_or(Ok(()), GitHubReviewRateLimitCheckpointV1::validate)?;
        if outcome == GitHubReviewIngressProviderOutcomeV1::Complete && self.next_cursor.is_some() {
            return Err(DomainError::NonCanonical {
                field: "complete github review cursor",
            });
        }
        if outcome == GitHubReviewIngressProviderOutcomeV1::RateLimited && self.rate_limit.is_none()
        {
            return Err(DomainError::NonCanonical {
                field: "github review rate-limit checkpoint",
            });
        }
        Ok(())
    }
}

/// Provider-observed rate-limit checkpoint. `remaining` may be zero, but it
/// can never exceed the provider's observed limit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewRateLimitCheckpointV1 {
    pub limit: u32,
    pub remaining: u32,
    pub reset_at: UtcMicros,
}

impl GitHubReviewRateLimitCheckpointV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.limit == 0 || self.remaining > self.limit {
            return Err(DomainError::NonCanonical {
                field: "github review rate-limit checkpoint",
            });
        }
        Ok(())
    }
}

/// Whether an original review anchor has a provable exact representation on
/// the current branch. Similar paths or lines alone never produce
/// [`Self::ExactCurrent`].
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReviewRemapStateV1 {
    ExactCurrent,
    Unmapped,
    Stale,
}

/// An immutable, generation-independent address captured from either the
/// original review position or a later exact current-branch projection.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewImmutableAnchorV1 {
    pub repository_id: RepositoryId,
    pub commit_id: CommitId,
    pub retrieval_anchor_id: RetrievalAnchorId,
    pub file: FileOccurrenceId,
    pub content_digest: ContentDigest,
    pub span: Option<SourceSpan>,
    pub symbol: Option<SymbolOccurrenceId>,
}

impl GitHubReviewImmutableAnchorV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.repository_id.validate()?;
        self.commit_id.validate()?;
        self.retrieval_anchor_id.validate()?;
        self.file.validate()?;
        self.content_digest.validate()?;
        self.span.as_ref().map_or(Ok(()), SourceSpan::validate)?;
        self.symbol
            .as_ref()
            .map_or(Ok(()), SymbolOccurrenceId::validate)
    }
}

/// Preserves the original observed review anchor and, only when provable,
/// stores a separate derived projection onto the current branch. Remapping
/// never mutates or replaces the original observed history.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewCurrentBranchRemapV1 {
    pub original: GitHubReviewImmutableAnchorV1,
    pub current_scope: FeedbackScopeV1,
    pub current: Option<GitHubReviewImmutableAnchorV1>,
    pub state: GitHubReviewRemapStateV1,
}

impl GitHubReviewCurrentBranchRemapV1 {
    /// Preserve an immutable original anchor when no exact current-branch
    /// projection exists. This deliberately never guesses a similar line.
    pub fn unmapped(
        original: GitHubReviewImmutableAnchorV1,
        current_scope: FeedbackScopeV1,
    ) -> Result<Self, DomainError> {
        let remap = Self {
            original,
            current_scope,
            current: None,
            state: GitHubReviewRemapStateV1::Unmapped,
        };
        remap.validate()?;
        Ok(remap)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.original.validate()?;
        self.current_scope.validate()?;
        if self.original.repository_id != self.current_scope.repository_id {
            return Err(DomainError::NonCanonical {
                field: "github review remap repository",
            });
        }

        match (&self.state, &self.current) {
            (GitHubReviewRemapStateV1::ExactCurrent, Some(current)) => {
                current.validate()?;
                if current.repository_id != self.current_scope.repository_id
                    || current.commit_id != self.current_scope.head_commit_id
                {
                    return Err(DomainError::NonCanonical {
                        field: "github review exact current anchor",
                    });
                }
            }
            (GitHubReviewRemapStateV1::ExactCurrent, None) => {
                return Err(DomainError::NonCanonical {
                    field: "github review exact current remap",
                });
            }
            (GitHubReviewRemapStateV1::Unmapped | GitHubReviewRemapStateV1::Stale, None) => {}
            (GitHubReviewRemapStateV1::Unmapped | GitHubReviewRemapStateV1::Stale, Some(_)) => {
                return Err(DomainError::NonCanonical {
                    field: "github review non-exact current anchor",
                });
            }
        }
        Ok(())
    }
}

/// One observed GitHub review comment or reply. The review lifecycle and
/// provider outcome are deliberately separate dimensions.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewItemV1 {
    pub provider: ProviderId,
    pub repository_id: RepositoryId,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub review_id: Option<GitHubReviewIdV1>,
    pub thread_id: Option<GitHubReviewThreadIdV1>,
    pub comment_id: GitHubReviewCommentIdV1,
    pub reply_to_comment_id: Option<GitHubReviewCommentIdV1>,
    pub version_digest: ManifestDigest,
    pub author_anchor: RetrievalAnchorId,
    pub author_class: GitHubReviewAuthorClassV1,
    pub review_state: GitHubReviewStateV1,
    pub body_digest: ManifestDigest,
    pub body_anchor: RetrievalAnchorId,
    pub safe_url_anchor: Option<RetrievalAnchorId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe_url: Option<String>,
    pub lifecycle: GitHubReviewLifecycleV1,
    pub provider_outcome: GitHubReviewIngressProviderOutcomeV1,
    pub remap: GitHubReviewCurrentBranchRemapV1,
    pub observed_at: UtcMicros,
}

impl GitHubReviewItemV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.provider.validate()?;
        self.repository_id.validate()?;
        self.pull_request_id.validate()?;
        self.review_id
            .as_ref()
            .map_or(Ok(()), GitHubReviewIdV1::validate)?;
        self.thread_id
            .as_ref()
            .map_or(Ok(()), GitHubReviewThreadIdV1::validate)?;
        self.comment_id.validate()?;
        self.reply_to_comment_id
            .as_ref()
            .map_or(Ok(()), GitHubReviewCommentIdV1::validate)?;
        self.version_digest.validate()?;
        self.author_anchor.validate()?;
        self.body_digest.validate()?;
        self.body_anchor.validate()?;
        self.safe_url_anchor
            .as_ref()
            .map_or(Ok(()), RetrievalAnchorId::validate)?;
        match (&self.safe_url_anchor, &self.safe_url) {
            (None, None) | (Some(_), None) => {}
            (Some(_), Some(value)) if safe_github_url(value) => {}
            _ => {
                return Err(DomainError::NonCanonical {
                    field: "github review safe URL",
                });
            }
        }
        self.remap.validate()?;
        if self.remap.original.repository_id != self.repository_id {
            return Err(DomainError::NonCanonical {
                field: "github review item repository",
            });
        }
        if self.lifecycle == GitHubReviewLifecycleV1::Current
            && self.remap.state != GitHubReviewRemapStateV1::ExactCurrent
        {
            return Err(DomainError::NonCanonical {
                field: "github review current lifecycle remap",
            });
        }
        Ok(())
    }
}

fn safe_github_url(value: &str) -> bool {
    if value.len() > 2_048 {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
}

/// Read-only connector output. Partial and stale outcomes may still include
/// previously observed items, whose lifecycle remains independently typed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewIngressResultV1 {
    pub provider: ProviderId,
    pub scope: FeedbackScopeV1,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub provider_base_commit_id: CommitId,
    pub provider_head_commit_id: CommitId,
    pub merge_base_commit_id: CommitId,
    pub operation: GitHubReviewReadOperationV1,
    pub outcome: GitHubReviewIngressProviderOutcomeV1,
    pub coverage: GitHubReviewCoverageV1,
    pub items: Vec<GitHubReviewItemV1>,
    pub fetched_at: UtcMicros,
}

impl GitHubReviewIngressResultV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.provider.validate()?;
        self.scope.validate()?;
        self.pull_request_id.validate()?;
        self.provider_base_commit_id.validate()?;
        self.provider_head_commit_id.validate()?;
        self.merge_base_commit_id.validate()?;
        if self.scope.head_commit_id != self.provider_head_commit_id
            && self.outcome != GitHubReviewIngressProviderOutcomeV1::Stale
        {
            return Err(DomainError::NonCanonical {
                field: "github review provider head commit",
            });
        }
        let coverage_matches = matches!(
            (self.outcome, self.coverage),
            (
                GitHubReviewIngressProviderOutcomeV1::Complete,
                GitHubReviewCoverageV1::Complete
            ) | (
                GitHubReviewIngressProviderOutcomeV1::Partial,
                GitHubReviewCoverageV1::Partial
            ) | (
                GitHubReviewIngressProviderOutcomeV1::Unavailable,
                GitHubReviewCoverageV1::Unavailable
            ) | (
                GitHubReviewIngressProviderOutcomeV1::Denied,
                GitHubReviewCoverageV1::Denied
            ) | (
                GitHubReviewIngressProviderOutcomeV1::Stale,
                GitHubReviewCoverageV1::Stale
            ) | (
                GitHubReviewIngressProviderOutcomeV1::RateLimited,
                GitHubReviewCoverageV1::Partial | GitHubReviewCoverageV1::Unavailable
            ) | (
                GitHubReviewIngressProviderOutcomeV1::Failed,
                GitHubReviewCoverageV1::Partial | GitHubReviewCoverageV1::Unavailable
            )
        );
        if !coverage_matches {
            return Err(DomainError::NonCanonical {
                field: "github review ingress coverage",
            });
        }
        for item in &self.items {
            item.validate()?;
            if item.provider != self.provider
                || item.repository_id != self.scope.repository_id
                || item.pull_request_id != self.pull_request_id
                || item.provider_outcome != self.outcome
                || item.remap.current_scope != self.scope
            {
                return Err(DomainError::NonCanonical {
                    field: "github review ingress item scope",
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_checkpoint_requires_observed_limit_state() {
        let missing = GitHubReviewReadCheckpointV1 {
            etag: None,
            next_cursor: None,
            rate_limit: None,
        };
        assert!(
            missing
                .validate_for(GitHubReviewIngressProviderOutcomeV1::RateLimited)
                .is_err()
        );

        let checkpoint = GitHubReviewReadCheckpointV1 {
            etag: Some(GitHubReviewEtagV1::new("W/\"fixture\"").unwrap()),
            next_cursor: Some(GitHubReviewCursorV1::new("cursor.fixture").unwrap()),
            rate_limit: Some(GitHubReviewRateLimitCheckpointV1 {
                limit: 5_000,
                remaining: 0,
                reset_at: UtcMicros(1),
            }),
        };
        checkpoint
            .validate_for(GitHubReviewIngressProviderOutcomeV1::RateLimited)
            .unwrap();

        assert!(
            checkpoint
                .validate_for(GitHubReviewIngressProviderOutcomeV1::Complete)
                .is_err(),
            "complete coverage cannot retain a next-page cursor"
        );
    }
}
