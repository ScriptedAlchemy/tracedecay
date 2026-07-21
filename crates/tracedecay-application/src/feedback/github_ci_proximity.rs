//! PR13 read-only/advisory feedback ingress contracts.
//!
//! GitHub review ingress accepts only a closed read-operation enum. This
//! application port deliberately exposes no generic HTTP request, mutation
//! operation, or write method, so an implementation receives no type that can
//! represent a GitHub network write.

use serde::{Deserialize, Serialize};
use tracedecay_domain::feedback::FeedbackScopeV1;
use tracedecay_domain::feedback::github_review::{
    GitHubPullRequestIdV1, GitHubReviewIngressResultV1, GitHubReviewReadOperationV1,
};

use crate::context::RequestContext;
use crate::error::ApplicationContractError;

use super::ports::FeedbackPortFuture;

/// Admission proof that the connector observed a credential limited to the
/// requested repository's read resources. Write-capable and indeterminate
/// scopes have no representable value and must fail before this request exists.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReadCredentialScopeV1 {
    VerifiedReadOnly,
}

/// Immutable, read-only ingress request for one pull request at one currently
/// resolved branch scope. There is no field for a generic endpoint, HTTP
/// method, GraphQL document, credential, or mutation payload.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewReadRequestV1 {
    pub operation: GitHubReviewReadOperationV1,
    pub scope: FeedbackScopeV1,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub credential_scope: GitHubReadCredentialScopeV1,
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

/// Read-only boundary for existing GitHub review comments, threads, and
/// replies. It has zero write methods by design. Implementations must reject
/// any network operation that cannot be constructed from
/// [`GitHubReviewReadOperationV1`] before credentials or network access.
pub trait GitHubReviewReadPort {
    fn read<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewIngressResultV1>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_operations_and_write_capable_credentials_are_not_deserializable() {
        assert!(serde_json::from_str::<GitHubReviewReadOperationV1>("\"mutation\"").is_err());
        assert!(serde_json::from_str::<GitHubReadCredentialScopeV1>("\"write_capable\"").is_err());
    }
}
