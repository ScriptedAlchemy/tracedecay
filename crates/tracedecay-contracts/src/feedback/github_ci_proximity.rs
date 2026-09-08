//! Read-only advisory feedback ingress contracts.
//!
//! The ports in this module preserve source-owned evidence only. They expose
//! neither generic network operations nor CI execution, scheduling, locking,
//! task assignment, or agent continuation.

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::feedback::{
    CiFailureLocalizationResultV1, CiFailureRateLimitCheckpointV1, CiFailureRunIdentityV1,
    CiFailureSourceFailureV1, FeedbackScopeV1, GitHubPullRequestIdV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1,
    GitHubReviewReadCheckpointV1, GitHubReviewReadOperationV1, ProximityContributionV1,
    ProximityInclusionV1,
};

use crate::context::RequestContext;
use crate::error::ApplicationContractError;

use super::ports::FeedbackPortFuture;

pub const GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1: &str =
    "capability.application.feedback.github-review-ingest";
pub const GITHUB_REVIEW_INGEST_USE_CASE_ID_V1: &str =
    "use-case.application.feedback.github-review-ingest";
pub const CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1: &str =
    "capability.application.feedback.ci-failure-localize";
pub const CI_FAILURE_LOCALIZE_USE_CASE_ID_V1: &str =
    "use-case.application.feedback.ci-failure-localize";
pub const PROXIMITY_CAPABILITY_ID_V1: &str = "capability.application.feedback.proximity";
pub const PROXIMITY_USE_CASE_ID_V1: &str = "use-case.application.feedback.proximity";
pub const ADVISORY_CYCLE_CAPABILITY_ID_V1: &str = "capability.application.feedback.advisory-cycle";
pub const ADVISORY_CYCLE_USE_CASE_ID_V1: &str = "use-case.application.feedback.advisory-cycle";

/// Immutable, read-only ingress request for one pull request at one currently
/// resolved branch scope. There is no field for a generic endpoint, HTTP
/// method, GraphQL document, credential, or mutation payload.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewReadRequestV1 {
    pub operation: GitHubReviewReadOperationV1,
    pub scope: FeedbackScopeV1,
    pub pull_request_id: GitHubPullRequestIdV1,
}

impl GitHubReviewReadRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.scope.validate()?;
        self.pull_request_id.validate()?;
        if !self.operation.is_read_only() {
            return Err(ApplicationContractError::Inconsistent {
                field: "github review read operation",
            });
        }
        Ok(())
    }
}

/// A validated read response combines source-owned review evidence with its
/// opaque cache/pagination/rate-limit checkpoint. It contains no credential,
/// endpoint, method, or mutation payload.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewReadResponseV1 {
    pub ingress: GitHubReviewIngressResultV1,
    pub checkpoint: GitHubReviewReadCheckpointV1,
}

impl GitHubReviewReadResponseV1 {
    pub fn validate_for(
        &self,
        request: &GitHubReviewReadRequestV1,
    ) -> Result<(), ApplicationContractError> {
        request.validate()?;
        self.ingress.validate()?;
        self.checkpoint.validate_for(self.ingress.outcome)?;
        let stale_has_checkpoint_evidence = self.ingress.provider_head_commit_id
            != self.ingress.scope.head_commit_id
            || self.checkpoint.etag.is_some()
            || self.checkpoint.next_cursor.is_some();
        if self.ingress.scope != request.scope
            || self.ingress.pull_request_id != request.pull_request_id
            || self.ingress.operation != request.operation
            || matches!(
                self.ingress.outcome,
                GitHubReviewIngressProviderOutcomeV1::Denied
                    | GitHubReviewIngressProviderOutcomeV1::Unavailable
            )
            || (self.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Stale
                && !stale_has_checkpoint_evidence)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "github review read response",
            });
        }
        Ok(())
    }
}

/// Transport-neutral result of attempting one already-admitted read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubReviewReadPortOutcomeV1 {
    Read(Box<GitHubReviewReadResponseV1>),
    Denied,
    Unavailable,
}

/// Read-only boundary for existing GitHub review comments, threads, and
/// replies. It has zero write methods by design. Implementations must reject
/// any network operation that cannot be constructed from
/// [`GitHubReviewReadOperationV1`] before credentials or network access.
pub trait GitHubReviewReadPort {
    fn read<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1>;
}

/// Immutable request to localize already-observed CI evidence. A run identity
/// is a provider record id, not an executable rerun handle.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CiFailureLocalizationRequestV1 {
    pub scope: FeedbackScopeV1,
    pub run: CiFailureRunIdentityV1,
}

impl CiFailureLocalizationRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.scope.validate()?;
        self.run.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CiFailureLocalizationPortOutcomeV1 {
    Localized(Box<CiFailureLocalizationResultV1>),
    RateLimited(CiFailureRateLimitCheckpointV1),
    Failed(CiFailureSourceFailureV1),
    Denied,
    Unavailable,
}

impl CiFailureLocalizationPortOutcomeV1 {
    pub fn validate_for(
        &self,
        request: &CiFailureLocalizationRequestV1,
    ) -> Result<(), ApplicationContractError> {
        request.validate()?;
        match self {
            Self::Localized(result) => {
                result.validate()?;
                if result.branch.scope != request.scope || result.run != request.run {
                    return Err(ApplicationContractError::Inconsistent {
                        field: "ci failure localization response",
                    });
                }
            }
            Self::RateLimited(checkpoint) => checkpoint.validate()?,
            Self::Failed(_) | Self::Denied | Self::Unavailable => {}
        }
        Ok(())
    }
}

/// Read-only CI localization port. There is intentionally no run, rerun, or
/// retry method.
pub trait CiFailureLocalizationPort {
    fn localize<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiFailureLocalizationPortOutcomeV1>;
}

/// Scope and time used by the single proximity producer to evaluate candidates.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProximityEvaluationRequestV1 {
    pub scope: FeedbackScopeV1,
    pub observed_at: UtcMicros,
}

impl ProximityEvaluationRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.scope.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProximityCandidatesPortOutcomeV1 {
    Candidates(Vec<ProximityContributionV1>),
    Denied,
    Unavailable,
}

impl ProximityCandidatesPortOutcomeV1 {
    pub fn validate_for(
        &self,
        request: &ProximityEvaluationRequestV1,
    ) -> Result<(), ApplicationContractError> {
        request.validate()?;
        let Self::Candidates(candidates) = self else {
            return Ok(());
        };
        for contribution in candidates {
            contribution.validate()?;
            if contribution.inclusion != ProximityInclusionV1::Included
                || contribution.is_expired_at(request.observed_at)
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "proximity candidate inclusion or expiry",
                });
            }
            if contribution
                .address
                .as_ref()
                .is_none_or(|address| address.scope != request.scope)
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "proximity candidate scope",
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProximityDedupeOutcomeV1 {
    Unique,
    Duplicate,
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_operations_are_not_deserializable() {
        assert!(serde_json::from_str::<GitHubReviewReadOperationV1>("\"mutation\"").is_err());
    }

    #[test]
    fn read_operation_families_are_closed_and_disjoint() {
        for operation in [
            GitHubReviewReadOperationV1::RestGetPullRequest,
            GitHubReviewReadOperationV1::RestListPullRequestReviews,
            GitHubReviewReadOperationV1::RestListPullRequestReviewComments,
        ] {
            assert!(operation.is_rest());
            assert!(!operation.is_graphql_query());
        }
        assert!(
            GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads.is_graphql_query()
        );
    }
}
