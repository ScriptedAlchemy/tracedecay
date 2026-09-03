//! Closed request and resume models for the read-only GitHub transport.

use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewCursorV1, GitHubReviewEtagV1,
    GitHubReviewRateLimitCheckpointV1, GitHubReviewReadCheckpointV1,
};

use crate::advisory::GitHubRestDescriptorV1;

/// Cache, pagination, and rate-limit state loaded from the injected durable
/// read checkpoint. It has no write precondition or mutation capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubReadResumeV1 {
    pub etag: Option<GitHubReviewEtagV1>,
    pub cursor: Option<GitHubReviewCursorV1>,
    pub rate_limit: Option<GitHubReviewRateLimitCheckpointV1>,
}

impl GitHubReadResumeV1 {
    pub fn empty() -> Self {
        Self {
            etag: None,
            cursor: None,
            rate_limit: None,
        }
    }

    pub fn from_checkpoint(checkpoint: GitHubReviewReadCheckpointV1) -> Option<Self> {
        if checkpoint
            .etag
            .as_ref()
            .is_some_and(|etag| etag.validate().is_err())
            || checkpoint
                .next_cursor
                .as_ref()
                .is_some_and(|cursor| cursor.validate().is_err())
            || checkpoint
                .rate_limit
                .as_ref()
                .is_some_and(|limit| limit.validate().is_err())
        {
            return None;
        }
        Some(Self {
            etag: checkpoint.etag,
            cursor: checkpoint.next_cursor,
            rate_limit: checkpoint.rate_limit,
        })
    }
}

/// The only REST-shaped request emitted by the runtime. The network authority
/// resolves its fixed endpoint from opaque scope and pull-request identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubRestReadRequestV1 {
    pub descriptor: GitHubRestDescriptorV1,
    pub scope: FeedbackScopeV1,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub resume: GitHubReadResumeV1,
}

/// The only GraphQL-shaped request emitted by the runtime. Query text is not
/// representable here; the concrete client owns one compile-time document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubGraphQlReadRequestV1 {
    pub scope: FeedbackScopeV1,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub resume: GitHubReadResumeV1,
}
