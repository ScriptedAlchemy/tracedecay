use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::json;
use tracedecay_application::feedback::FeedbackPortFuture;
#[cfg(test)]
use tracedecay_application::feedback::GitHubReviewReadRequestV1;
use tracedecay_application::{RequestAdmission, RequestContext, now_micros};
use tracedecay_domain::feedback::{
    GitHubReviewCursorV1, GitHubReviewEtagV1, GitHubReviewRateLimitCheckpointV1,
    GitHubReviewReadOperationV1,
};
use tracedecay_domain::{UserProfileId, UtcMicros};
use url::Url;
use zeroize::Zeroizing;

use super::dto::{
    GraphQlCommentPageNodeV1, GraphQlResponseV1, RestPullRequestV1, RestReviewCommentV1,
    RestReviewV1,
};
use super::{
    GitHubGraphQlReadRequestV1, GitHubReadNetworkMetadataV1, GitHubReadNetworkOutcomeV1,
    GitHubReadNetworkResponseV1, GitHubReadNetworkStatusV1, GitHubReadOnlyNetworkAuthorityV1,
    GitHubRestReadRequestV1, MAX_GITHUB_READ_RESPONSE_BYTES_V1,
};

pub const GITHUB_REVIEW_THREADS_QUERY_V1: &str = r"
query TraceDecayPR13ReviewThreads(
  $owner: String!
  $repository: String!
  $number: Int!
  $threadAfter: String
  $commentThreadId: ID!
  $commentAfter: String
  $loadThreads: Boolean!
  $loadComments: Boolean!
) {
  repository(owner: $owner, name: $repository) @include(if: $loadThreads) {
    pullRequest(number: $number) {
      baseRefOid
      headRefOid
      reviewThreads(first: 100, after: $threadAfter) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated path line originalLine startLine originalStartLine
          comments(first: 100) {
            pageInfo { hasNextPage endCursor }
            nodes {
              databaseId url bodyText createdAt updatedAt authorAssociation
              replyTo { databaseId }
              author { __typename login }
              pullRequestReview { databaseId state commit { oid } }
              originalCommit { oid }
            }
          }
        }
      }
    }
  }
  node(id: $commentThreadId) @include(if: $loadComments) {
    ... on PullRequestReviewThread {
      id
      comments(first: 100, after: $commentAfter) {
        pageInfo { hasNextPage endCursor }
        nodes {
          databaseId url bodyText createdAt updatedAt authorAssociation
          replyTo { databaseId }
          author { __typename login }
          pullRequestReview { databaseId state commit { oid } }
          originalCommit { oid }
        }
      }
    }
  }
}
";

const MAX_REVIEW_ITEMS_V1: usize = 2_000;
const MAX_NESTED_COMMENT_PAGES_V1: usize = 20;
const MAX_REVIEW_SCAN_PAGES_V1: u32 = 20;
const MAX_CI_RESPONSE_BYTES_V1: usize = 2 * 1024 * 1024;

mod ci_network;
mod credential;
mod model;
mod review;
mod transport;

pub use ci_network::{GitHubCiReadOnlyClientV1, GitHubCiTransportOutcomeV1};
use credential::GitHubCredentialAuthorizationV1;
#[cfg(test)]
use credential::ProfileGitHubReadOnlyCredentialAuthorityV1;
pub use credential::{
    GitHubReadOnlyCredentialAuthorityOutcomeV1, GitHubReadOnlyCredentialAuthorityV1,
    GitHubReadOnlyCredentialSecretV1, GitHubReadOnlyCredentialV1, GitHubReadPermissionV1,
    ProfileGitHubReadOnlyCredentialMountOutcomeV1, RegisteredGitHubReadOnlyCredentialV1,
    mount_profile_github_read_only_credential_authority_v1,
    register_github_read_only_credential_authority_v1,
    register_profile_github_public_repository_v1,
    register_profile_github_read_only_credential_authority_v1,
    resolve_registered_github_read_only_credential_v1,
    unmount_profile_github_read_only_credential_authority_v1,
    unregister_github_read_only_credential_authority_v1,
    unregister_profile_github_public_repository_v1,
    unregister_profile_github_read_only_credential_authority_v1,
};
pub use model::{GitHubCiRepositoryTargetV1, GitHubHttpReadConfigV1, GitHubRepositoryTargetV1};
pub use review::GitHubReadOnlyClientV1;
pub use transport::GitHubHttpReadClientV1;
pub(in crate::advisory::github_runtime) use transport::HttpResponseV1;
use transport::{
    merge_rate_limit, network_failure, page_from_cursor, parse_bounded, request_context_admitted,
    valid_ci_page, valid_full_commit_id, valid_path_segment,
};

#[cfg(test)]
#[path = "network/tests/pagination.rs"]
mod pagination_contract_tests;
#[cfg(test)]
#[path = "network/tests/mod.rs"]
mod tests;
