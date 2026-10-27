//! Concrete, injected runtime transport for the closed GitHub review-read port.
//!
//! The concrete owner admits exact scope and source access before invoking a
//! client that exposes only fixed REST GETs and one static GraphQL query.

mod access;
mod anchors;
mod ci_cache;
mod credential_lifecycle;
mod decoder;
mod discovery;
mod dto;
mod gh_cli;
mod network;
mod owner;
mod protocol;
mod rate_gate;
mod read_requests;
mod releases;
mod stack;
mod stack_anchors;
mod store;

pub use stack_anchors::{
    GitHubStackAnchorPublicationOutcomeV1, GitHubStackAnchorReadOutcomeV1,
    ProjectGitHubStackAnchorAuthorityV1,
};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracedecay_contracts::RequestContext;
use tracedecay_contracts::feedback::{
    FeedbackPortFuture, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
    GitHubReviewReadPort, GitHubReviewReadPortOutcomeV1, GitHubReviewReadRequestV1,
    GitHubReviewReadResponseV1,
};
use tracedecay_domain::feedback::{
    GitHubReviewCoverageV1, GitHubReviewCursorV1, GitHubReviewEtagV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1,
    GitHubReviewRateLimitCheckpointV1, GitHubReviewReadCheckpointV1, GitHubReviewReadOperationV1,
};
use tracedecay_domain::{ManifestDigest, canonical_sha256};

use super::{GitHubReadOnlyTransport, GitHubRestDescriptorV1, context_allows_feedback_operation};

pub use access::{
    ConfiguredGitHubSourceAccessAuthorityV1, DAEMON_GITHUB_ORIGIN_SOURCE_BINDING_ID,
    daemon_owned_github_source_binding_v1, github_repository_from_remote_v1,
};
pub use anchors::{
    GitHubReviewBodyEvidenceAuthorityV1, GitHubReviewBodyEvidenceV1, GitHubReviewBodyReadOutcomeV1,
    ProjectGitHubAnchorAuthorityV1, ProjectGitHubRegistrarAuthoritiesV1,
    github_anchor_authorities_arc_v1,
};
pub use credential_lifecycle::{
    GitHubReadOnlyCredentialLifecycleV1, GitHubReadOnlyCredentialPermissionVerifierV1,
    GitHubReviewSourceFindingV1, GitHubSecretReadErrorV1, GitHubSecretReadPortV1,
    check_configured_github_review_sources_v1,
};
pub use decoder::{
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCanonicalReviewAnchorsV1,
    GitHubOfficialResponseDecoderV1, GitHubReviewAnchorSeedV1, GitHubReviewProviderIdentityV1,
    MAX_GITHUB_REVIEW_BODY_BYTES_V1,
};
pub use discovery::{
    GitHubDiscoveryControlV1, GitHubExactCommitDiscoveryOutcomeV1, GitHubExactCommitPullRequestV1,
    GitHubPullRequestDiscoveryKindV1, GitHubSourceReadingReceiptV1, GitHubSourceReadingV1,
    GitHubSourceStatusV1, GitHubSourceUnobservedV1, discover_exact_commit_pull_request_v1,
    github_source_reading_v1, github_source_state_v1,
    record_github_source_advisory_mount_failure_v1, record_github_source_reading_v1,
};
pub use dto::{
    GitHubActionsCheckRunOutputV1, GitHubActionsCheckRunV1, GitHubActionsCheckSuiteRefV1,
    GitHubActionsConclusionV1, GitHubActionsPullRequestRefV1, GitHubActionsStatusV1,
    GitHubActionsWorkflowJobV1, GitHubActionsWorkflowRunV1, GitHubActionsWorkflowStepV1,
    GitHubCheckAnnotationLevelV1, GitHubCheckAnnotationV1, GitHubRetainedResponseV1,
};
#[cfg(any(test, feature = "test-transport"))]
pub(crate) use dto::{GraphQlResponseV1, RestPullRequestV1, RestReviewCommentV1, RestReviewV1};
pub use gh_cli::{public_repository_read_authorization_v1, public_repository_read_credential_v1};
#[cfg(test)]
pub(crate) use network::ci_fixture_client;
pub use network::{
    GITHUB_REVIEW_THREADS_QUERY_V1, GitHubCiReadOnlyClientV1, GitHubCiRepositoryTargetV1,
    GitHubCiTransportOutcomeV1, GitHubHttpReadConfigV1, GitHubReadOnlyClientV1,
    GitHubReadOnlyCredentialAuthorityOutcomeV1, GitHubReadOnlyCredentialAuthorityV1,
    GitHubReadOnlyCredentialSecretV1, GitHubReadOnlyCredentialV1, GitHubReadPermissionV1,
    GitHubRepositoryTargetV1, register_github_read_only_credential_authority_v1,
    register_profile_github_read_only_credential_authority_v1,
    unregister_github_read_only_credential_authority_v1,
    unregister_profile_github_read_only_credential_authority_v1,
};
pub use network::{
    ProfileGitHubReadOnlyCredentialMountOutcomeV1, RegisteredGitHubReadOnlyCredentialV1,
    mount_profile_github_read_only_credential_authority_v1,
    register_profile_github_public_repository_v1,
    resolve_registered_github_read_only_credential_v1,
    unmount_profile_github_read_only_credential_authority_v1,
    unregister_profile_github_public_repository_v1,
};
pub use owner::{
    GitHubReviewRuntimeOwnerBuildErrorV1, GitHubReviewRuntimeOwnerConfigV1,
    GitHubReviewRuntimeOwnerV1, GitHubStackObservabilityV1, build_github_review_runtime_owner_v1,
};
pub use read_requests::{GitHubGraphQlReadRequestV1, GitHubReadResumeV1, GitHubRestReadRequestV1};
pub use releases::{
    GitHubReleaseAssetV1, GitHubReleaseReadControlV1, GitHubReleaseTagV1, GitHubReleaseV1,
    ProjectGitHubReleaseAuthorityOpenOutcomeV1, ProjectGitHubReleasePageV1,
    ProjectGitHubReleaseReadAuthorityV1, ProjectGitHubReleaseReadOutcomeV1,
    ProjectGitHubReleaseReadRequestV1, open_project_github_release_read_authority_v1,
};
pub use store::{
    GitHubReviewStoreManifestEntryV1, GitHubReviewStoreManifestLoadOutcomeV1,
    GitHubReviewStoreManifestV1, MAX_GITHUB_REVIEW_STORE_MANIFEST_ENTRIES_V1,
    ProjectGitHubReviewStoreV1,
};

/// Raw GitHub response bytes are transient parser input only. They are never
/// put into a checkpoint, an ingress result, or this transport's receipt.
pub const MAX_GITHUB_READ_RESPONSE_BYTES_V1: usize = 1024 * 1024;
const MAX_GITHUB_REFRESH_SCAN_PAGES_V1: usize = 20;
const GITHUB_REVIEW_SCAN_TOKEN_DOMAIN_V1: &str = "tracedecay.advisory.github.scan-token.v1";
const MAX_GITHUB_REFRESH_ATTEMPT_RECEIPTS_V1: usize = 64;

/// Read response metadata retained by the domain checkpoint. It deliberately
/// does not expose arbitrary headers, a status code, or a redirect location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubReadNetworkMetadataV1 {
    pub status: GitHubReadNetworkStatusV1,
    pub etag: Option<GitHubReviewEtagV1>,
    pub next_cursor: Option<GitHubReviewCursorV1>,
    pub rate_limit: Option<GitHubReviewRateLimitCheckpointV1>,
    pub retry_at: Option<tracedecay_domain::UtcMicros>,
}

impl GitHubReadNetworkMetadataV1 {
    fn validate(&self) -> bool {
        self.etag
            .as_ref()
            .is_none_or(|etag| etag.validate().is_ok())
            && self
                .next_cursor
                .as_ref()
                .is_none_or(|cursor| cursor.validate().is_ok())
            && self
                .rate_limit
                .as_ref()
                .is_none_or(|limit| limit.validate().is_ok())
            && match self.status {
                GitHubReadNetworkStatusV1::RateLimited => {
                    self.rate_limit.is_some() || self.retry_at.is_some()
                }
                GitHubReadNetworkStatusV1::Ok | GitHubReadNetworkStatusV1::NotModified => {
                    self.retry_at.is_none()
                }
            }
    }
}

/// A closed set of successful/read-side network states. Neither an arbitrary
/// HTTP method nor a redirect/write state can be represented here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubReadNetworkStatusV1 {
    Ok,
    NotModified,
    RateLimited,
}

/// Bounded transient response from the injected network authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubReadNetworkResponseV1 {
    pub metadata: GitHubReadNetworkMetadataV1,
    pub body: Vec<u8>,
}

impl GitHubReadNetworkResponseV1 {
    fn validate(&self) -> bool {
        self.body.len() <= MAX_GITHUB_READ_RESPONSE_BYTES_V1
            && self.metadata.validate()
            && (self.metadata.status != GitHubReadNetworkStatusV1::NotModified
                || self.body.is_empty())
    }
}

/// Network failure and authorization are intentionally distinct from an
/// ingress response. The only read-side outcome that carries bytes is
/// [`Self::Response`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubReadNetworkOutcomeV1 {
    Response(GitHubReadNetworkResponseV1),
    Denied,
    Unavailable,
}

/// Daemon/store-owned checkpoint authority. It has no mutation method because
/// final checkpoint persistence is owned by the authoritative ingress commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubReadCheckpointLoadOutcomeV1 {
    Checkpoint(GitHubReviewReadCheckpointV1),
    Empty,
    Unavailable,
}

pub trait GitHubReadCheckpointAuthorityV1 {
    fn load_resume<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReadCheckpointLoadOutcomeV1>;
}

/// Injected network authority with exactly two non-mutating operations. The
/// GraphQL method is named `query`, not a generic HTTP verb, to make mutation
/// construction impossible in this runtime provider.
pub trait GitHubReadOnlyNetworkAuthorityV1 {
    fn get<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubRestReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1>;

    fn query<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubGraphQlReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1>;
}

/// Transient page result. Withheld optional prose is distinct from pagination
/// so a later complete page cannot upgrade an incomplete collection to ready.
pub struct GitHubDecodedReadResponseV1 {
    pub ingress: GitHubReviewIngressResultV1,
    pub withheld_optional_prose: bool,
}

/// Parser/normalizer for bounded transient response bytes. It receives no
/// credentials and returns the source-owned domain result plus page coverage.
pub trait GitHubReadResponseDecoderV1 {
    fn decode<'a>(
        &'a self,
        request: &'a GitHubReviewReadRequestV1,
        metadata: &'a GitHubReadNetworkMetadataV1,
        body: &'a [u8],
    ) -> FeedbackPortFuture<'a, Option<GitHubDecodedReadResponseV1>>;
}

/// Concrete runtime implementation of the existing read-only transport port.
/// All network, checkpoint, and response-decoding authorities are injected;
/// daemon wiring chooses their implementations later.
pub struct GitHubReadOnlyRuntimeTransportV1<C, N, D> {
    checkpoints: C,
    network: N,
    decoder: D,
}

#[derive(Clone, Copy)]
enum GitHubFullScanRouteV1 {
    Rest(GitHubRestDescriptorV1),
    GraphQl,
}

impl<C, N, D> GitHubReadOnlyRuntimeTransportV1<C, N, D> {
    pub fn new(checkpoints: C, network: N, decoder: D) -> Self {
        Self {
            checkpoints,
            network,
            decoder,
        }
    }
}

impl<C, N, D> GitHubReadOnlyRuntimeTransportV1<C, N, D>
where
    C: GitHubReadCheckpointAuthorityV1 + Sync,
    N: GitHubReadOnlyNetworkAuthorityV1 + Sync,
    D: GitHubReadResponseDecoderV1 + Sync,
{
    async fn decode_outcome(
        &self,
        request: &GitHubReviewReadRequestV1,
        resume: GitHubReadResumeV1,
        outcome: GitHubReadNetworkOutcomeV1,
    ) -> Result<(GitHubReviewReadResponseV1, bool), GitHubReviewReadPortOutcomeV1> {
        let response = match outcome {
            GitHubReadNetworkOutcomeV1::Response(response) => response,
            GitHubReadNetworkOutcomeV1::Denied => {
                return Err(GitHubReviewReadPortOutcomeV1::Denied);
            }
            GitHubReadNetworkOutcomeV1::Unavailable => {
                return Err(GitHubReviewReadPortOutcomeV1::Unavailable);
            }
        };
        if !response.validate() {
            return Err(GitHubReviewReadPortOutcomeV1::Unavailable);
        }
        let Some(GitHubDecodedReadResponseV1 {
            ingress,
            withheld_optional_prose,
        }) = self
            .decoder
            .decode(request, &response.metadata, &response.body)
            .await
        else {
            return Err(GitHubReviewReadPortOutcomeV1::Unavailable);
        };
        if ingress.operation != request.operation
            || ingress.scope != request.scope
            || ingress.pull_request_id != request.pull_request_id
            || !network_status_matches(response.metadata.status, ingress.outcome)
            || (withheld_optional_prose
                && (response.metadata.status != GitHubReadNetworkStatusV1::Ok
                    || ingress.outcome != GitHubReviewIngressProviderOutcomeV1::Partial))
        {
            return Err(GitHubReviewReadPortOutcomeV1::Unavailable);
        }
        let checkpoint = GitHubReviewReadCheckpointV1 {
            etag: response.metadata.etag.or(resume.etag),
            next_cursor: if response.metadata.status == GitHubReadNetworkStatusV1::Ok {
                // A successful terminal page ends the scan even when its
                // optional prose was withheld and coverage remains partial.
                response.metadata.next_cursor
            } else {
                response.metadata.next_cursor.or(resume.cursor)
            },
            rate_limit: response.metadata.rate_limit.or(resume.rate_limit),
        };
        let response = GitHubReviewReadResponseV1 {
            ingress,
            checkpoint,
            completed_collection_scan: false,
        };
        if response.validate_for(request).is_ok() {
            Ok((response, withheld_optional_prose))
        } else {
            Err(GitHubReviewReadPortOutcomeV1::Unavailable)
        }
    }

    async fn resume_for(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
    ) -> Option<GitHubReadResumeV1> {
        match self.checkpoints.load_resume(context, request).await {
            GitHubReadCheckpointLoadOutcomeV1::Checkpoint(checkpoint) => {
                GitHubReadResumeV1::from_checkpoint(checkpoint)
            }
            GitHubReadCheckpointLoadOutcomeV1::Empty => Some(GitHubReadResumeV1::empty()),
            GitHubReadCheckpointLoadOutcomeV1::Unavailable => None,
        }
    }

    async fn full_scan(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
        mut resume: GitHubReadResumeV1,
        route: GitHubFullScanRouteV1,
    ) -> GitHubReviewReadPortOutcomeV1 {
        if resume.cursor.take().is_some() {
            resume.etag = None;
        }
        let mut items = BTreeMap::new();
        let mut conversation = BTreeMap::new();
        let mut reviews = BTreeMap::new();
        let mut quarantined = BTreeMap::new();
        let mut withheld_optional_prose = false;
        let mut visited_cursors = std::collections::BTreeSet::new();
        for _ in 0..MAX_GITHUB_REFRESH_SCAN_PAGES_V1 {
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReviewReadPortOutcomeV1::Denied;
            }
            let outcome = match route {
                GitHubFullScanRouteV1::Rest(descriptor) => {
                    self.network
                        .get(
                            context,
                            &GitHubRestReadRequestV1 {
                                descriptor,
                                scope: request.scope.clone(),
                                pull_request_id: request.pull_request_id.clone(),
                                resume: resume.clone(),
                            },
                        )
                        .await
                }
                GitHubFullScanRouteV1::GraphQl => {
                    self.network
                        .query(
                            context,
                            &GitHubGraphQlReadRequestV1 {
                                scope: request.scope.clone(),
                                pull_request_id: request.pull_request_id.clone(),
                                resume: resume.clone(),
                            },
                        )
                        .await
                }
            };
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReviewReadPortOutcomeV1::Denied;
            }
            let (mut response, withheld_on_page) =
                match self.decode_outcome(request, resume.clone(), outcome).await {
                    Ok(decoded) => decoded,
                    Err(outcome) => return outcome,
                };
            withheld_optional_prose |= withheld_on_page;
            let next_cursor = response.checkpoint.next_cursor.clone();
            for item in response.ingress.items.drain(..) {
                if quarantined.contains_key(item.comment_id.as_str())
                    || items
                        .insert(item.comment_id.as_str().to_owned(), item)
                        .is_some()
                {
                    return GitHubReviewReadPortOutcomeV1::Unavailable;
                }
            }
            for comment in response.ingress.conversation.drain(..) {
                if quarantined.contains_key(comment.comment_id.as_str())
                    || conversation
                        .insert(comment.comment_id.as_str().to_owned(), comment)
                        .is_some()
                {
                    return GitHubReviewReadPortOutcomeV1::Unavailable;
                }
            }
            for review in response.ingress.reviews.drain(..) {
                if reviews
                    .insert(review.review_id.as_str().to_owned(), review)
                    .is_some()
                {
                    return GitHubReviewReadPortOutcomeV1::Unavailable;
                }
            }
            for item in response.ingress.quarantined.drain(..) {
                if items.contains_key(item.comment_id.as_str())
                    || conversation.contains_key(item.comment_id.as_str())
                    || quarantined
                        .insert(item.comment_id.as_str().to_owned(), item)
                        .is_some()
                {
                    return GitHubReviewReadPortOutcomeV1::Unavailable;
                }
            }
            if let Some(cursor) = next_cursor {
                if response.ingress.outcome != GitHubReviewIngressProviderOutcomeV1::Partial
                    || !visited_cursors.insert(cursor.as_str().to_owned())
                {
                    return GitHubReviewReadPortOutcomeV1::Unavailable;
                }
                resume = GitHubReadResumeV1 {
                    etag: None,
                    cursor: Some(cursor),
                    rate_limit: response.checkpoint.rate_limit,
                };
                continue;
            }
            if !(items.is_empty()
                && conversation.is_empty()
                && reviews.is_empty()
                && quarantined.is_empty())
                && response.ingress.outcome != GitHubReviewIngressProviderOutcomeV1::Complete
                && !(withheld_optional_prose
                    && response.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Partial)
            {
                return GitHubReviewReadPortOutcomeV1::Unavailable;
            }
            if withheld_optional_prose {
                response.ingress.outcome = GitHubReviewIngressProviderOutcomeV1::Partial;
                response.ingress.coverage = GitHubReviewCoverageV1::Partial;
            }
            response.ingress.items = items.into_values().collect();
            response.ingress.conversation = conversation.into_values().collect();
            response.ingress.reviews = reviews.into_values().collect();
            response.ingress.quarantined = quarantined.into_values().collect();
            for item in &mut response.ingress.items {
                item.provider_outcome = response.ingress.outcome;
            }
            response.checkpoint.next_cursor = None;
            response.completed_collection_scan = matches!(
                request.operation,
                GitHubReviewReadOperationV1::RestListPullRequestReviews
                    | GitHubReviewReadOperationV1::RestListPullRequestConversationComments
            ) && (response.ingress.outcome
                == GitHubReviewIngressProviderOutcomeV1::Complete
                || withheld_optional_prose);
            if !visited_cursors.is_empty() || withheld_optional_prose {
                // A page-specific validator cannot revalidate this aggregate
                // or an incomplete prose snapshot as the last complete read.
                response.checkpoint.etag = None;
            }
            return if response.validate_for(request).is_ok() {
                GitHubReviewReadPortOutcomeV1::Read(Box::new(response))
            } else {
                GitHubReviewReadPortOutcomeV1::Unavailable
            };
        }
        GitHubReviewReadPortOutcomeV1::Unavailable
    }
}

impl<C, N, D> GitHubReadOnlyTransport for GitHubReadOnlyRuntimeTransportV1<C, N, D>
where
    C: GitHubReadCheckpointAuthorityV1 + Sync,
    N: GitHubReadOnlyNetworkAuthorityV1 + Sync,
    D: GitHubReadResponseDecoderV1 + Sync,
{
    fn rest_get<'a>(
        &'a self,
        context: &'a RequestContext,
        descriptor: GitHubRestDescriptorV1,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1> {
        Box::pin(async move {
            if request.validate().is_err()
                || descriptor.validate().is_err()
                || descriptor.operation != request.operation
            {
                return GitHubReviewReadPortOutcomeV1::Unavailable;
            }
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReviewReadPortOutcomeV1::Denied;
            }
            let Some(resume) = self.resume_for(context, request).await else {
                if !context_allows_feedback_operation(
                    context,
                    &request.scope,
                    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                    GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
                ) {
                    return GitHubReviewReadPortOutcomeV1::Denied;
                }
                return GitHubReviewReadPortOutcomeV1::Unavailable;
            };
            self.full_scan(
                context,
                request,
                resume,
                GitHubFullScanRouteV1::Rest(descriptor),
            )
            .await
        })
    }

    fn graphql_query<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1> {
        Box::pin(async move {
            if request.validate().is_err() || !request.operation.is_graphql_query() {
                return GitHubReviewReadPortOutcomeV1::Unavailable;
            }
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReviewReadPortOutcomeV1::Denied;
            }
            let Some(resume) = self.resume_for(context, request).await else {
                if !context_allows_feedback_operation(
                    context,
                    &request.scope,
                    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                    GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
                ) {
                    return GitHubReviewReadPortOutcomeV1::Denied;
                }
                return GitHubReviewReadPortOutcomeV1::Unavailable;
            };
            self.full_scan(context, request, resume, GitHubFullScanRouteV1::GraphQl)
                .await
        })
    }
}

fn network_status_matches(
    status: GitHubReadNetworkStatusV1,
    outcome: GitHubReviewIngressProviderOutcomeV1,
) -> bool {
    match status {
        GitHubReadNetworkStatusV1::Ok => !matches!(
            outcome,
            GitHubReviewIngressProviderOutcomeV1::RateLimited
                | GitHubReviewIngressProviderOutcomeV1::Unavailable
                | GitHubReviewIngressProviderOutcomeV1::Denied
        ),
        GitHubReadNetworkStatusV1::NotModified => {
            outcome == GitHubReviewIngressProviderOutcomeV1::Stale
        }
        GitHubReadNetworkStatusV1::RateLimited => {
            outcome == GitHubReviewIngressProviderOutcomeV1::RateLimited
        }
    }
}

const GITHUB_REVIEW_REFRESH_STATE_DOMAIN_V1: &str = "tracedecay.advisory.github.refresh-state.v1";

fn github_review_refresh_revision(
    last_complete: Option<&GitHubReviewReadResponseV1>,
    latest_attempt: &GitHubReviewReadResponseV1,
    attempt_receipts: &[GitHubReviewRefreshAttemptReceiptV1],
    last_partial_collection: Option<&GitHubReviewPartialCollectionGenerationV1>,
) -> Option<ManifestDigest> {
    if let Some(collection) = last_partial_collection {
        canonical_sha256(&(
            GITHUB_REVIEW_REFRESH_STATE_DOMAIN_V1,
            last_complete,
            latest_attempt,
            attempt_receipts,
            collection,
        ))
        .ok()
    } else if attempt_receipts.is_empty() {
        canonical_sha256(&(
            GITHUB_REVIEW_REFRESH_STATE_DOMAIN_V1,
            last_complete,
            latest_attempt,
        ))
        .ok()
    } else {
        canonical_sha256(&(
            GITHUB_REVIEW_REFRESH_STATE_DOMAIN_V1,
            last_complete,
            latest_attempt,
            attempt_receipts,
        ))
        .ok()
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubReviewRefreshAttemptDispositionV1 {
    Terminal,
    Agreed,
    RetryAgreed,
    Quarantined,
}

/// Durable evidence for one bounded refresh acquisition attempt. Scan digests
/// are content-equivalence tokens, never provider or publication identity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewRefreshAttemptReceiptV1 {
    pub disposition: GitHubReviewRefreshAttemptDispositionV1,
    pub scan_digests: Vec<ManifestDigest>,
    pub observed_at: tracedecay_domain::UtcMicros,
}

impl GitHubReviewRefreshAttemptReceiptV1 {
    fn validate(&self) -> bool {
        if self
            .scan_digests
            .iter()
            .any(|digest| digest.validate().is_err())
        {
            return false;
        }
        match self.disposition {
            GitHubReviewRefreshAttemptDispositionV1::Terminal => self.scan_digests.len() == 1,
            GitHubReviewRefreshAttemptDispositionV1::Agreed => {
                self.scan_digests.len() == 2
                    && self.scan_digests.first() == self.scan_digests.get(1)
            }
            GitHubReviewRefreshAttemptDispositionV1::RetryAgreed => {
                self.scan_digests.len() == 3
                    && self.scan_digests.first() != self.scan_digests.get(1)
                    && self.scan_digests.get(1) == self.scan_digests.get(2)
            }
            GitHubReviewRefreshAttemptDispositionV1::Quarantined => {
                self.scan_digests.len() == 3
                    && self.scan_digests.first() != self.scan_digests.get(1)
                    && self.scan_digests.get(1) != self.scan_digests.get(2)
            }
        }
    }

    fn agrees_with(
        &self,
        request: &GitHubReviewReadRequestV1,
        response: &GitHubReviewReadResponseV1,
    ) -> bool {
        self.validate()
            && matches!(
                self.disposition,
                GitHubReviewRefreshAttemptDispositionV1::Agreed
                    | GitHubReviewRefreshAttemptDispositionV1::RetryAgreed
            )
            && self.observed_at == response.ingress.fetched_at
            && github_review_scan_digest(request, response)
                .as_ref()
                .is_some_and(|digest| self.scan_digests.last() == Some(digest))
    }
}

/// The latest agreed collection whose optional prose remains partial. Its
/// membership and proof survive later incomplete attempts and receipt-ring
/// pruning without claiming a complete prose generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewPartialCollectionGenerationV1 {
    pub response: GitHubReviewReadResponseV1,
    pub receipt: GitHubReviewRefreshAttemptReceiptV1,
}

impl GitHubReviewPartialCollectionGenerationV1 {
    fn validate_for(&self, request: &GitHubReviewReadRequestV1) -> bool {
        self.response.completed_collection_scan
            && self.response.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Partial
            && self.receipt.agrees_with(request, &self.response)
    }
}

/// A complete canonical item set and its cursor/checkpoint are one durable
/// generation. No partial, stale, denied, or unavailable attempt can be
/// represented as a complete generation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewCompleteGenerationV1 {
    pub response: GitHubReviewReadResponseV1,
}

impl GitHubReviewCompleteGenerationV1 {
    pub fn from_response(
        request: &GitHubReviewReadRequestV1,
        response: GitHubReviewReadResponseV1,
    ) -> Option<Self> {
        if response.validate_for(request).is_err()
            || response.ingress.outcome != GitHubReviewIngressProviderOutcomeV1::Complete
        {
            return None;
        }
        Some(Self { response })
    }

    fn validate_for(&self, request: &GitHubReviewReadRequestV1) -> bool {
        Self::from_response(request, self.response.clone()).is_some()
    }
}

/// Durable refresh state keeps the latest ingress attempt separate from the
/// last complete generation. The latest attempt may be partial, rate-limited,
/// stale, or failed without replacing complete canonical observations.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewRefreshStateV1 {
    pub revision: ManifestDigest,
    pub last_complete: Option<GitHubReviewCompleteGenerationV1>,
    pub latest_attempt: GitHubReviewReadResponseV1,
    #[serde(default)]
    pub attempt_receipts: Vec<GitHubReviewRefreshAttemptReceiptV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_partial_collection: Option<GitHubReviewPartialCollectionGenerationV1>,
}

impl GitHubReviewRefreshStateV1 {
    pub fn transition(
        request: &GitHubReviewReadRequestV1,
        previous: Option<&Self>,
        latest_attempt: GitHubReviewReadResponseV1,
    ) -> Option<Self> {
        Self::transition_with_receipt(request, previous, latest_attempt, None)
    }

    fn transition_with_receipt(
        request: &GitHubReviewReadRequestV1,
        previous: Option<&Self>,
        latest_attempt: GitHubReviewReadResponseV1,
        receipt: Option<GitHubReviewRefreshAttemptReceiptV1>,
    ) -> Option<Self> {
        if latest_attempt.validate_for(request).is_err()
            || previous.is_some_and(|state| !state.validate_for(request))
            || receipt.as_ref().is_some_and(|receipt| !receipt.validate())
        {
            return None;
        }
        let acquired_collection = receipt.as_ref().and_then(|receipt| {
            let generation = GitHubReviewPartialCollectionGenerationV1 {
                response: latest_attempt.clone(),
                receipt: receipt.clone(),
            };
            generation.validate_for(request).then_some(generation)
        });
        let previous_collection = previous.and_then(|state| state.last_partial_collection.as_ref());
        if (acquired_collection.is_some()
            || latest_attempt.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Complete)
            && previous_collection.is_some_and(|collection| {
                latest_attempt.ingress.fetched_at < collection.response.ingress.fetched_at
            })
        {
            return None;
        }
        let latest_attempt =
            normalize_refresh_attempt(request, previous, latest_attempt, receipt.as_ref())?;
        let last_complete =
            if latest_attempt.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Complete {
                Some(GitHubReviewCompleteGenerationV1::from_response(
                    request,
                    latest_attempt.clone(),
                )?)
            } else {
                previous.and_then(|state| state.last_complete.clone())
            };
        let last_partial_collection =
            if latest_attempt.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Complete {
                None
            } else {
                acquired_collection.or_else(|| previous_collection.cloned())
            };
        let mut attempt_receipts = previous.map_or_default(|state| state.attempt_receipts.clone());
        if let Some(receipt) = receipt {
            attempt_receipts.push(receipt);
            if attempt_receipts.len() > MAX_GITHUB_REFRESH_ATTEMPT_RECEIPTS_V1 {
                attempt_receipts
                    .drain(..attempt_receipts.len() - MAX_GITHUB_REFRESH_ATTEMPT_RECEIPTS_V1);
            }
        }
        let revision = github_review_refresh_revision(
            last_complete
                .as_ref()
                .map(|generation| &generation.response),
            &latest_attempt,
            &attempt_receipts,
            last_partial_collection.as_ref(),
        )?;
        let state = Self {
            revision,
            last_complete,
            latest_attempt,
            attempt_receipts,
            last_partial_collection,
        };
        state.validate_for(request).then_some(state)
    }

    pub fn validate_for(&self, request: &GitHubReviewReadRequestV1) -> bool {
        if self.latest_attempt.validate_for(request).is_err()
            || self.attempt_receipts.len() > MAX_GITHUB_REFRESH_ATTEMPT_RECEIPTS_V1
            || self
                .attempt_receipts
                .iter()
                .any(|receipt| !receipt.validate())
            || self
                .last_complete
                .as_ref()
                .is_some_and(|generation| !generation.validate_for(request))
            || self
                .last_partial_collection
                .as_ref()
                .is_some_and(|collection| {
                    !collection.validate_for(request)
                        || collection.response.ingress.provider
                            != self.latest_attempt.ingress.provider
                        || collection.response.ingress.fetched_at
                            > self.latest_attempt.ingress.fetched_at
                        || self.latest_attempt.ingress.outcome
                            == GitHubReviewIngressProviderOutcomeV1::Complete
                        || self.last_complete.as_ref().is_some_and(|complete| {
                            complete.response.ingress.fetched_at
                                > collection.response.ingress.fetched_at
                        })
                })
        {
            return false;
        }
        if self.latest_attempt.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Complete
            && self
                .last_complete
                .as_ref()
                .is_none_or(|generation| generation.response != self.latest_attempt)
        {
            return false;
        }
        github_review_refresh_revision(
            self.last_complete
                .as_ref()
                .map(|generation| &generation.response),
            &self.latest_attempt,
            &self.attempt_receipts,
            self.last_partial_collection.as_ref(),
        )
        .is_some_and(|expected| expected == self.revision)
    }
}

fn normalize_refresh_attempt(
    request: &GitHubReviewReadRequestV1,
    previous: Option<&GitHubReviewRefreshStateV1>,
    mut latest: GitHubReviewReadResponseV1,
    receipt: Option<&GitHubReviewRefreshAttemptReceiptV1>,
) -> Option<GitHubReviewReadResponseV1> {
    if latest.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Stale {
        // A partial prose collection has no aggregate validator. A later
        // opaque 304 must not revalidate the older complete generation and
        // resurrect membership that the agreed collection removed.
        if previous.is_some_and(|state| state.last_partial_collection.is_some()) {
            return None;
        }
        let revalidated_at = latest.ingress.fetched_at;
        let checkpoint = latest.checkpoint;
        let previous_complete = previous?.last_complete.as_ref()?;
        latest = previous_complete.response.clone();
        latest.ingress.fetched_at = revalidated_at;
        latest.checkpoint = checkpoint;
        for item in &mut latest.ingress.items {
            item.provider_outcome = GitHubReviewIngressProviderOutcomeV1::Complete;
        }
    }
    let mut items = BTreeMap::new();
    let mut conversation = BTreeMap::new();
    let mut reviews = BTreeMap::new();
    let mut quarantined = BTreeMap::new();
    let previous_partial = previous
        .map(|state| &state.latest_attempt)
        .filter(|response| {
            response.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Partial
                && response.ingress.operation == latest.ingress.operation
        });
    let previous_collection = previous.and_then(|state| state.last_partial_collection.as_ref());
    let replaces_collection = latest.ingress.outcome
        == GitHubReviewIngressProviderOutcomeV1::Complete
        || (latest.completed_collection_scan
            && latest.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Partial
            && receipt.is_some_and(|receipt| receipt.agrees_with(request, &latest)));
    if !replaces_collection {
        // A proven collection remains the membership baseline through rate
        // limits, disagreement and continued pages. Never fall back to the
        // older complete prose generation after this baseline removed IDs.
        if let Some(collection) = previous_collection {
            for comment in &collection.response.ingress.conversation {
                conversation.insert(comment.comment_id.as_str().to_owned(), comment.clone());
            }
            for review in &collection.response.ingress.reviews {
                reviews.insert(review.review_id.as_str().to_owned(), review.clone());
            }
        }
        let retained_attempt = previous_partial.or_else(|| {
            previous_collection.and_then(|_| previous.map(|state| &state.latest_attempt))
        });
        if let Some(retained) = retained_attempt {
            for comment in &retained.ingress.conversation {
                conversation.insert(comment.comment_id.as_str().to_owned(), comment.clone());
            }
            for review in &retained.ingress.reviews {
                reviews.insert(review.review_id.as_str().to_owned(), review.clone());
            }
        }
        latest.completed_collection_scan = false;
    }
    if let Some(previous_partial) = previous_partial {
        for mut item in previous_partial.ingress.items.clone() {
            item.provider_outcome = latest.ingress.outcome;
            items.insert(item.comment_id.as_str().to_owned(), item);
        }
        for item in previous_partial.ingress.quarantined.clone() {
            quarantined.insert(item.comment_id.as_str().to_owned(), item);
        }
    }
    for item in latest.ingress.items.drain(..) {
        quarantined.remove(item.comment_id.as_str());
        items.insert(item.comment_id.as_str().to_owned(), item);
    }
    for comment in latest.ingress.conversation.drain(..) {
        quarantined.remove(comment.comment_id.as_str());
        conversation.insert(comment.comment_id.as_str().to_owned(), comment);
    }
    for review in latest.ingress.reviews.drain(..) {
        reviews.insert(review.review_id.as_str().to_owned(), review);
    }
    for item in latest.ingress.quarantined.drain(..) {
        conversation.remove(item.comment_id.as_str());
        items.remove(item.comment_id.as_str());
        quarantined.insert(item.comment_id.as_str().to_owned(), item);
    }

    if latest.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Complete
        && let Some(previous_complete) = previous
            .and_then(|state| state.last_complete.as_ref())
            .map(|generation| &generation.response)
            .filter(|response| response.ingress.operation == latest.ingress.operation)
    {
        for prior in &previous_complete.ingress.items {
            match items.get_mut(prior.comment_id.as_str()) {
                Some(current)
                    if current.lifecycle
                        != tracedecay_domain::feedback::GitHubReviewLifecycleV1::Resolved
                        && current.version_digest != prior.version_digest =>
                {
                    current.lifecycle =
                        tracedecay_domain::feedback::GitHubReviewLifecycleV1::Edited;
                }
                Some(_) => {}
                None if quarantined.contains_key(prior.comment_id.as_str()) => {}
                None => {
                    let mut deleted = prior.clone();
                    deleted.lifecycle =
                        tracedecay_domain::feedback::GitHubReviewLifecycleV1::Deleted;
                    deleted.provider_outcome = GitHubReviewIngressProviderOutcomeV1::Complete;
                    deleted.observed_at = latest.ingress.fetched_at;
                    items.insert(deleted.comment_id.as_str().to_owned(), deleted);
                }
            }
        }
    }
    latest.ingress.items = items.into_values().collect();
    latest.ingress.conversation = conversation.into_values().collect();
    latest.ingress.reviews = reviews.into_values().collect();
    latest.ingress.quarantined = quarantined.into_values().collect();
    if previous_partial.is_some() {
        // A collection ETag is specific to the page URL that emitted it.
        // Never retain a continuation-page ETag as authority for page one of
        // the next refresh.
        latest.checkpoint.etag = None;
    }
    latest.validate_for(request).is_ok().then_some(latest)
}

fn github_review_scan_digest(
    request: &GitHubReviewReadRequestV1,
    response: &GitHubReviewReadResponseV1,
) -> Option<ManifestDigest> {
    response.validate_for(request).ok()?;
    let mut stable = response.clone();
    stable.ingress.fetched_at = tracedecay_domain::UtcMicros(0);
    stable.checkpoint.etag = None;
    stable.checkpoint.rate_limit = None;
    stable.ingress.items.sort_by(|left, right| {
        left.comment_id
            .as_str()
            .cmp(right.comment_id.as_str())
            .then_with(|| {
                left.version_digest
                    .as_str()
                    .cmp(right.version_digest.as_str())
            })
    });
    for item in &mut stable.ingress.items {
        item.observed_at = tracedecay_domain::UtcMicros(0);
    }
    canonical_sha256(&(GITHUB_REVIEW_SCAN_TOKEN_DOMAIN_V1, stable)).ok()
}

fn quarantined_refresh_attempt(
    request: &GitHubReviewReadRequestV1,
    mut response: GitHubReviewReadResponseV1,
) -> Option<GitHubReviewReadResponseV1> {
    response.ingress.outcome = GitHubReviewIngressProviderOutcomeV1::Partial;
    response.ingress.coverage = tracedecay_domain::feedback::GitHubReviewCoverageV1::Partial;
    for item in &mut response.ingress.items {
        item.provider_outcome = GitHubReviewIngressProviderOutcomeV1::Partial;
    }
    response.checkpoint.etag = None;
    response.checkpoint.next_cursor = None;
    response.completed_collection_scan = false;
    response.validate_for(request).ok().map(|()| response)
}

fn requires_refresh_scan_consensus(response: &GitHubReviewReadResponseV1) -> bool {
    response.completed_collection_scan
        || matches!(
            response.ingress.outcome,
            GitHubReviewIngressProviderOutcomeV1::Complete
                | GitHubReviewIngressProviderOutcomeV1::Stale
        )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubReviewRefreshStoreReadOutcomeV1 {
    State(Box<GitHubReviewRefreshStateV1>),
    Empty,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubReviewRefreshStoreCommitOutcomeV1 {
    Recorded,
    Duplicate,
    Conflict,
    Unavailable,
}

/// The store must compare `expected_revision` and record the complete
/// state in one serialized transaction. This is the sole durable write in a
/// refresh; observations and cursor cannot commit independently.
pub trait GitHubReviewAtomicRefreshStoreV1 {
    fn load<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreReadOutcomeV1>;

    fn compare_and_record<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
        expected_revision: Option<&'a ManifestDigest>,
        next: &'a GitHubReviewRefreshStateV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreCommitOutcomeV1>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubReviewRefreshReceiptV1 {
    pub state: GitHubReviewRefreshStateV1,
    pub deleted_items: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubReviewRefreshOutcomeV1 {
    Stored(Box<GitHubReviewRefreshReceiptV1>),
    Cancelled,
    Denied,
    Stale,
    Unavailable,
}

impl GitHubReviewRefreshOutcomeV1 {
    /// A retained refusal is useful status evidence, but must not trigger
    /// additional provider reads as if the collection refresh succeeded.
    pub fn has_usable_observation(&self) -> bool {
        matches!(self, Self::Stored(receipt) if matches!(
            receipt.state.latest_attempt.ingress.outcome,
            GitHubReviewIngressProviderOutcomeV1::Complete | GitHubReviewIngressProviderOutcomeV1::Partial
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubProviderLifecycleV1 {
    Ready,
    Denied,
    Stale,
    Ambiguous,
    Unavailable,
}

pub trait GitHubSourceAccessAuthorityV1: Send + Sync {
    fn authorize<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubProviderLifecycleV1>;
}

impl GitHubSourceAccessAuthorityV1 for std::sync::Arc<dyn GitHubSourceAccessAuthorityV1> {
    fn authorize<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubProviderLifecycleV1> {
        self.as_ref().authorize(context, request)
    }
}

/// One explicit, non-repeating refresh. A compare conflict is surfaced as
/// stale rather than retried, so this coordinator cannot become a polling or
/// autonomous ingestion loop.
pub struct GitHubReviewRefreshCoordinatorV1<P, S, A> {
    port: P,
    store: S,
    source_access: A,
}

impl<P, S, A> GitHubReviewRefreshCoordinatorV1<P, S, A> {
    pub fn new(port: P, store: S, source_access: A) -> Self {
        Self {
            port,
            store,
            source_access,
        }
    }
}

impl<P, S, A> GitHubReviewRefreshCoordinatorV1<P, S, A>
where
    P: GitHubReviewReadPort + Sync,
    S: GitHubReviewAtomicRefreshStoreV1 + Sync,
    A: GitHubSourceAccessAuthorityV1,
{
    pub fn refresh<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshOutcomeV1> {
        Box::pin(async move {
            if request.validate().is_err() {
                return GitHubReviewRefreshOutcomeV1::Unavailable;
            }
            if let Some(outcome) = refresh_request_outcome(context, request) {
                return outcome;
            }
            if let Some(outcome) =
                blocked_refresh_outcome(self.source_access.authorize(context, request).await)
            {
                return outcome;
            }
            if let Some(outcome) = refresh_request_outcome(context, request) {
                return outcome;
            }
            let previous = match self.store.load(context, request).await {
                GitHubReviewRefreshStoreReadOutcomeV1::State(state) => {
                    if !state.validate_for(request) {
                        return GitHubReviewRefreshOutcomeV1::Unavailable;
                    }
                    Some(state)
                }
                GitHubReviewRefreshStoreReadOutcomeV1::Empty => None,
                GitHubReviewRefreshStoreReadOutcomeV1::Unavailable => {
                    if let Some(outcome) = refresh_request_outcome(context, request) {
                        return outcome;
                    }
                    return GitHubReviewRefreshOutcomeV1::Unavailable;
                }
            };
            let first_read = self.port.read(context, request).await;
            if let Some(outcome) = refresh_request_outcome(context, request) {
                return outcome;
            }
            if let Some(outcome) =
                blocked_refresh_outcome(self.source_access.authorize(context, request).await)
            {
                return outcome;
            }
            if let Some(outcome) = refresh_request_outcome(context, request) {
                return outcome;
            }
            let GitHubReviewReadPortOutcomeV1::Read(first_response) = first_read else {
                return match first_read {
                    GitHubReviewReadPortOutcomeV1::Denied => GitHubReviewRefreshOutcomeV1::Denied,
                    GitHubReviewReadPortOutcomeV1::Unavailable => {
                        GitHubReviewRefreshOutcomeV1::Unavailable
                    }
                    GitHubReviewReadPortOutcomeV1::Read(_) => unreachable!(),
                };
            };
            let first_response = *first_response;
            let Some(first_digest) = github_review_scan_digest(request, &first_response) else {
                return GitHubReviewRefreshOutcomeV1::Unavailable;
            };
            let (latest_attempt, receipt, quarantined) =
                if requires_refresh_scan_consensus(&first_response) {
                    let second_read = self.port.read(context, request).await;
                    if let Some(outcome) = refresh_request_outcome(context, request) {
                        return outcome;
                    }
                    if let Some(outcome) = blocked_refresh_outcome(
                        self.source_access.authorize(context, request).await,
                    ) {
                        return outcome;
                    }
                    if let Some(outcome) = refresh_request_outcome(context, request) {
                        return outcome;
                    }
                    let GitHubReviewReadPortOutcomeV1::Read(second_response) = second_read else {
                        return match second_read {
                            GitHubReviewReadPortOutcomeV1::Denied => {
                                GitHubReviewRefreshOutcomeV1::Denied
                            }
                            GitHubReviewReadPortOutcomeV1::Unavailable => {
                                GitHubReviewRefreshOutcomeV1::Unavailable
                            }
                            GitHubReviewReadPortOutcomeV1::Read(_) => unreachable!(),
                        };
                    };
                    let second_response = *second_response;
                    let Some(second_digest) = github_review_scan_digest(request, &second_response)
                    else {
                        return GitHubReviewRefreshOutcomeV1::Unavailable;
                    };
                    if first_digest == second_digest {
                        let observed_at = first_response
                            .ingress
                            .fetched_at
                            .max(second_response.ingress.fetched_at);
                        (
                            second_response,
                            GitHubReviewRefreshAttemptReceiptV1 {
                                disposition: GitHubReviewRefreshAttemptDispositionV1::Agreed,
                                scan_digests: vec![first_digest, second_digest],
                                observed_at,
                            },
                            false,
                        )
                    } else {
                        let third_read = self.port.read(context, request).await;
                        if let Some(outcome) = refresh_request_outcome(context, request) {
                            return outcome;
                        }
                        if let Some(outcome) = blocked_refresh_outcome(
                            self.source_access.authorize(context, request).await,
                        ) {
                            return outcome;
                        }
                        if let Some(outcome) = refresh_request_outcome(context, request) {
                            return outcome;
                        }
                        let GitHubReviewReadPortOutcomeV1::Read(third_response) = third_read else {
                            return match third_read {
                                GitHubReviewReadPortOutcomeV1::Denied => {
                                    GitHubReviewRefreshOutcomeV1::Denied
                                }
                                GitHubReviewReadPortOutcomeV1::Unavailable => {
                                    GitHubReviewRefreshOutcomeV1::Unavailable
                                }
                                GitHubReviewReadPortOutcomeV1::Read(_) => unreachable!(),
                            };
                        };
                        let third_response = *third_response;
                        let Some(third_digest) =
                            github_review_scan_digest(request, &third_response)
                        else {
                            return GitHubReviewRefreshOutcomeV1::Unavailable;
                        };
                        if second_digest == third_digest {
                            let observed_at = third_response.ingress.fetched_at;
                            (
                                third_response,
                                GitHubReviewRefreshAttemptReceiptV1 {
                                    disposition:
                                        GitHubReviewRefreshAttemptDispositionV1::RetryAgreed,
                                    scan_digests: vec![first_digest, second_digest, third_digest],
                                    observed_at,
                                },
                                false,
                            )
                        } else {
                            let Some(quarantined_response) =
                                quarantined_refresh_attempt(request, third_response.clone())
                            else {
                                return GitHubReviewRefreshOutcomeV1::Unavailable;
                            };
                            (
                                quarantined_response,
                                GitHubReviewRefreshAttemptReceiptV1 {
                                    disposition:
                                        GitHubReviewRefreshAttemptDispositionV1::Quarantined,
                                    scan_digests: vec![first_digest, second_digest, third_digest],
                                    observed_at: third_response.ingress.fetched_at,
                                },
                                true,
                            )
                        }
                    }
                } else {
                    (
                        first_response.clone(),
                        GitHubReviewRefreshAttemptReceiptV1 {
                            disposition: GitHubReviewRefreshAttemptDispositionV1::Terminal,
                            scan_digests: vec![first_digest],
                            observed_at: first_response.ingress.fetched_at,
                        },
                        false,
                    )
                };
            let Some(next) = GitHubReviewRefreshStateV1::transition_with_receipt(
                request,
                previous.as_deref(),
                latest_attempt,
                Some(receipt),
            ) else {
                return GitHubReviewRefreshOutcomeV1::Unavailable;
            };
            let expected = previous.as_ref().map(|state| &state.revision);
            if let Some(outcome) = refresh_request_outcome(context, request) {
                return outcome;
            }
            if let Some(outcome) =
                blocked_refresh_outcome(self.source_access.authorize(context, request).await)
            {
                return outcome;
            }
            if let Some(outcome) = refresh_request_outcome(context, request) {
                return outcome;
            }
            let outcome = self
                .store
                .compare_and_record(context, request, expected, &next)
                .await;
            if let Some(outcome) = refresh_request_outcome(context, request) {
                return outcome;
            }
            if let Some(outcome) =
                blocked_refresh_outcome(self.source_access.authorize(context, request).await)
            {
                return outcome;
            }
            if let Some(outcome) = refresh_request_outcome(context, request) {
                return outcome;
            }
            match outcome {
                GitHubReviewRefreshStoreCommitOutcomeV1::Recorded
                | GitHubReviewRefreshStoreCommitOutcomeV1::Duplicate => {
                    if quarantined {
                        return GitHubReviewRefreshOutcomeV1::Stale;
                    }
                    let deleted_items = next.last_complete.as_ref().map_or(0, |generation| {
                        generation
                            .response
                            .ingress
                            .items
                            .iter()
                            .filter(|item| {
                                item.lifecycle
                                    == tracedecay_domain::feedback::GitHubReviewLifecycleV1::Deleted
                            })
                            .count() as u64
                    });
                    GitHubReviewRefreshOutcomeV1::Stored(Box::new(GitHubReviewRefreshReceiptV1 {
                        state: next,
                        deleted_items,
                    }))
                }
                GitHubReviewRefreshStoreCommitOutcomeV1::Conflict => {
                    GitHubReviewRefreshOutcomeV1::Stale
                }
                GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable => {
                    GitHubReviewRefreshOutcomeV1::Unavailable
                }
            }
        })
    }
}

fn refresh_request_outcome(
    context: &RequestContext,
    request: &GitHubReviewReadRequestV1,
) -> Option<GitHubReviewRefreshOutcomeV1> {
    if context.cancellation().is_cancelled() {
        Some(GitHubReviewRefreshOutcomeV1::Cancelled)
    } else if !context_allows_feedback_operation(
        context,
        &request.scope,
        GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
    ) {
        Some(GitHubReviewRefreshOutcomeV1::Denied)
    } else {
        None
    }
}

fn blocked_refresh_outcome(
    lifecycle: GitHubProviderLifecycleV1,
) -> Option<GitHubReviewRefreshOutcomeV1> {
    match lifecycle {
        GitHubProviderLifecycleV1::Ready => None,
        GitHubProviderLifecycleV1::Denied => Some(GitHubReviewRefreshOutcomeV1::Denied),
        GitHubProviderLifecycleV1::Stale | GitHubProviderLifecycleV1::Ambiguous => {
            Some(GitHubReviewRefreshOutcomeV1::Stale)
        }
        GitHubProviderLifecycleV1::Unavailable => Some(GitHubReviewRefreshOutcomeV1::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_contracts::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::feedback::{
        FeedbackScopeV1, GitHubConversationCommentV1, GitHubPullRequestReviewV1,
        GitHubReviewAuthorClassV1, GitHubReviewCoverageV1, GitHubReviewCurrentBranchRemapV1,
        GitHubReviewImmutableAnchorV1, GitHubReviewReadOperationV1, GitHubReviewStateV1,
    };
    use tracedecay_domain::{
        ActorId, CommitId, GitHubPullRequestIdV1, GitHubReviewCommentIdV1, GitHubReviewIdV1,
        ManifestDigest, ProjectId, ProviderId, RefId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use crate::advisory::{
        GitHubCurrentBranchRemapper, GitHubReadOnlyConnector, GitHubReadOnlyDescriptorSetV1,
    };

    use super::*;

    const SHA: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[derive(Clone)]
    struct Checkpoints(Option<GitHubReviewReadCheckpointV1>);

    impl GitHubReadCheckpointAuthorityV1 for Checkpoints {
        fn load_resume<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a GitHubReviewReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReadCheckpointLoadOutcomeV1> {
            Box::pin(async move {
                self.0.clone().map_or(
                    GitHubReadCheckpointLoadOutcomeV1::Empty,
                    GitHubReadCheckpointLoadOutcomeV1::Checkpoint,
                )
            })
        }
    }

    #[derive(Default)]
    struct NetworkCalls {
        get: AtomicUsize,
        query: AtomicUsize,
        last_rest: Mutex<Option<GitHubRestReadRequestV1>>,
        last_query: Mutex<Option<GitHubGraphQlReadRequestV1>>,
    }

    struct Network {
        calls: Arc<NetworkCalls>,
        outcome: GitHubReadNetworkOutcomeV1,
    }

    impl GitHubReadOnlyNetworkAuthorityV1 for Network {
        fn get<'a>(
            &'a self,
            _context: &'a RequestContext,
            request: &'a GitHubRestReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1> {
            self.calls.get.fetch_add(1, Ordering::SeqCst);
            *self.calls.last_rest.lock().unwrap() = Some(request.clone());
            let outcome = self.outcome.clone();
            Box::pin(async move { outcome })
        }

        fn query<'a>(
            &'a self,
            _context: &'a RequestContext,
            request: &'a GitHubGraphQlReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1> {
            self.calls.query.fetch_add(1, Ordering::SeqCst);
            *self.calls.last_query.lock().unwrap() = Some(request.clone());
            let outcome = self.outcome.clone();
            Box::pin(async move { outcome })
        }
    }

    struct PageNetwork {
        outcomes: Mutex<VecDeque<GitHubReadNetworkOutcomeV1>>,
        cursors: Mutex<Vec<Option<String>>>,
        calls: AtomicUsize,
    }

    impl GitHubReadOnlyNetworkAuthorityV1 for PageNetwork {
        fn get<'a>(
            &'a self,
            _context: &'a RequestContext,
            request: &'a GitHubRestReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.cursors.lock().unwrap().push(
                request
                    .resume
                    .cursor
                    .as_ref()
                    .map(|cursor| cursor.as_str().to_owned()),
            );
            let outcome = self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(GitHubReadNetworkOutcomeV1::Unavailable);
            Box::pin(async move { outcome })
        }

        fn query<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a GitHubGraphQlReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1> {
            Box::pin(async { GitHubReadNetworkOutcomeV1::Unavailable })
        }
    }

    struct Decoder;

    impl GitHubReadResponseDecoderV1 for Decoder {
        fn decode<'a>(
            &'a self,
            request: &'a GitHubReviewReadRequestV1,
            metadata: &'a GitHubReadNetworkMetadataV1,
            _body: &'a [u8],
        ) -> FeedbackPortFuture<'a, Option<GitHubDecodedReadResponseV1>> {
            Box::pin(async move {
                let (outcome, coverage) = match metadata.status {
                    GitHubReadNetworkStatusV1::Ok if metadata.next_cursor.is_some() => (
                        GitHubReviewIngressProviderOutcomeV1::Partial,
                        GitHubReviewCoverageV1::Partial,
                    ),
                    GitHubReadNetworkStatusV1::Ok => (
                        GitHubReviewIngressProviderOutcomeV1::Complete,
                        GitHubReviewCoverageV1::Complete,
                    ),
                    GitHubReadNetworkStatusV1::NotModified => (
                        GitHubReviewIngressProviderOutcomeV1::Stale,
                        GitHubReviewCoverageV1::Stale,
                    ),
                    GitHubReadNetworkStatusV1::RateLimited => (
                        GitHubReviewIngressProviderOutcomeV1::RateLimited,
                        GitHubReviewCoverageV1::Partial,
                    ),
                };
                Some(GitHubDecodedReadResponseV1 {
                    ingress: GitHubReviewIngressResultV1 {
                        provider: ProviderId::new("provider.github.runtime").ok()?,
                        scope: request.scope.clone(),
                        pull_request_id: request.pull_request_id.clone(),
                        provider_base_commit_id: CommitId::new("commit.github.base").ok()?,
                        provider_head_commit_id: request.scope.head_commit_id.clone(),
                        merge_base_commit_id: CommitId::new("commit.github.merge-base").ok()?,
                        operation: request.operation,
                        outcome,
                        coverage,
                        items: Vec::new(),
                        quarantined: Vec::new(),
                        conversation: Vec::new(),
                        reviews: Vec::new(),
                        pull_request: None,
                        fetched_at: UtcMicros(10),
                    },
                    withheld_optional_prose: false,
                })
            })
        }
    }

    struct Reads {
        outcomes: Mutex<VecDeque<GitHubReviewReadPortOutcomeV1>>,
        calls: AtomicUsize,
    }

    impl Reads {
        fn new(outcomes: Vec<GitHubReviewReadPortOutcomeV1>) -> Self {
            Self {
                outcomes: Mutex::new(outcomes.into()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl GitHubReviewReadPort for Reads {
        fn read<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a GitHubReviewReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let outcome = self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(GitHubReviewReadPortOutcomeV1::Unavailable);
            Box::pin(async move { outcome })
        }
    }

    impl GitHubReviewReadPort for &Reads {
        fn read<'a>(
            &'a self,
            context: &'a RequestContext,
            request: &'a GitHubReviewReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1> {
            <Reads as GitHubReviewReadPort>::read(*self, context, request)
        }
    }

    #[derive(Default)]
    struct RefreshStore {
        state: Mutex<Option<GitHubReviewRefreshStateV1>>,
        commits: AtomicUsize,
    }

    impl GitHubReviewAtomicRefreshStoreV1 for RefreshStore {
        fn load<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a GitHubReviewReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreReadOutcomeV1> {
            let state = self.state.lock().unwrap().clone();
            Box::pin(async move {
                state.map_or(GitHubReviewRefreshStoreReadOutcomeV1::Empty, |state| {
                    GitHubReviewRefreshStoreReadOutcomeV1::State(Box::new(state))
                })
            })
        }

        fn compare_and_record<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a GitHubReviewReadRequestV1,
            expected_revision: Option<&'a ManifestDigest>,
            next: &'a GitHubReviewRefreshStateV1,
        ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreCommitOutcomeV1> {
            let expected_revision = expected_revision.cloned();
            let next = next.clone();
            Box::pin(async move {
                let mut state = self.state.lock().unwrap();
                if state.as_ref().map(|state| &state.revision) != expected_revision.as_ref() {
                    return GitHubReviewRefreshStoreCommitOutcomeV1::Conflict;
                }
                *state = Some(next);
                self.commits.fetch_add(1, Ordering::SeqCst);
                GitHubReviewRefreshStoreCommitOutcomeV1::Recorded
            })
        }
    }

    impl GitHubReviewAtomicRefreshStoreV1 for &RefreshStore {
        fn load<'a>(
            &'a self,
            context: &'a RequestContext,
            request: &'a GitHubReviewReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreReadOutcomeV1> {
            <RefreshStore as GitHubReviewAtomicRefreshStoreV1>::load(*self, context, request)
        }

        fn compare_and_record<'a>(
            &'a self,
            context: &'a RequestContext,
            request: &'a GitHubReviewReadRequestV1,
            expected_revision: Option<&'a ManifestDigest>,
            next: &'a GitHubReviewRefreshStateV1,
        ) -> FeedbackPortFuture<'a, GitHubReviewRefreshStoreCommitOutcomeV1> {
            <RefreshStore as GitHubReviewAtomicRefreshStoreV1>::compare_and_record(
                *self,
                context,
                request,
                expected_revision,
                next,
            )
        }
    }

    struct Ready;

    impl GitHubSourceAccessAuthorityV1 for Ready {
        fn authorize<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a GitHubReviewReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubProviderLifecycleV1> {
            Box::pin(async { GitHubProviderLifecycleV1::Ready })
        }
    }

    fn context_and_request(
        operation: GitHubReviewReadOperationV1,
    ) -> (RequestContext, GitHubReviewReadRequestV1) {
        let project_id = ProjectId::new("project.github.runtime").unwrap();
        let repository_id = RepositoryId::new("repository.github.runtime").unwrap();
        let worktree_id = WorktreeId::new("worktree.github.runtime").unwrap();
        let resolved_scope = ResolvedScope::new(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            Some(RefId::new("refs/heads/github-runtime").unwrap()),
        )
        .unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.github.runtime").unwrap(),
            1,
            ManifestDigest::new(SHA).unwrap(),
            ActorId::new("actor.github.runtime.issuer").unwrap(),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            resolved_scope.clone(),
            BTreeSet::from([CapabilityId::new(
                "capability.application.feedback.github-review-ingest",
            )
            .unwrap()]),
            BTreeSet::from([
                UseCaseId::new("use-case.application.feedback.github-review-ingest").unwrap(),
            ]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        let context = RequestContext::new(
            ActorId::new("actor.github.runtime").unwrap(),
            resolved_scope,
            grant,
            RequestId::new("request.github.runtime").unwrap(),
            Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
            CancellationContext::active("cancel.github.runtime").unwrap(),
        )
        .unwrap();
        let scope = FeedbackScopeV1 {
            project_id,
            repository_id,
            worktree_id,
            branch_ref: "refs/heads/github-runtime".to_owned(),
            head_commit_id: CommitId::new("commit.github.head").unwrap(),
        };
        (
            context,
            GitHubReviewReadRequestV1 {
                operation,
                scope,
                pull_request_id: GitHubPullRequestIdV1::new("pull-request.github.runtime").unwrap(),
            },
        )
    }

    fn rate_limited_response() -> GitHubReadNetworkOutcomeV1 {
        GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
            metadata: GitHubReadNetworkMetadataV1 {
                status: GitHubReadNetworkStatusV1::RateLimited,
                etag: Some(GitHubReviewEtagV1::new("W/\"runtime\"").unwrap()),
                next_cursor: None,
                rate_limit: Some(GitHubReviewRateLimitCheckpointV1 {
                    limit: 60,
                    remaining: 0,
                    reset_at: UtcMicros(100),
                }),
                retry_at: None,
            },
            body: Vec::new(),
        })
    }

    fn complete_response(request: &GitHubReviewReadRequestV1) -> GitHubReviewReadResponseV1 {
        GitHubReviewReadResponseV1 {
            completed_collection_scan: false,
            ingress: GitHubReviewIngressResultV1 {
                provider: ProviderId::new("github").unwrap(),
                scope: request.scope.clone(),
                pull_request_id: request.pull_request_id.clone(),
                provider_base_commit_id: CommitId::new("commit.github.base").unwrap(),
                provider_head_commit_id: request.scope.head_commit_id.clone(),
                merge_base_commit_id: CommitId::new("commit.github.merge-base").unwrap(),
                operation: request.operation,
                outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
                coverage: GitHubReviewCoverageV1::Complete,
                items: Vec::new(),
                quarantined: Vec::new(),
                conversation: Vec::new(),
                reviews: Vec::new(),
                pull_request: None,
                fetched_at: UtcMicros(11),
            },
            checkpoint: GitHubReviewReadCheckpointV1 {
                etag: None,
                next_cursor: None,
                rate_limit: None,
            },
        }
    }

    fn read(response: GitHubReviewReadResponseV1) -> GitHubReviewReadPortOutcomeV1 {
        GitHubReviewReadPortOutcomeV1::Read(Box::new(response))
    }

    #[tokio::test]
    async fn refresh_requires_two_stable_equivalent_full_scans() {
        let (context, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviewComments);
        let first = complete_response(&request);
        let mut second = first.clone();
        second.ingress.fetched_at = UtcMicros(first.ingress.fetched_at.0 + 1);
        let reads = Reads::new(vec![read(first), read(second)]);
        let store = RefreshStore::default();
        let coordinator = GitHubReviewRefreshCoordinatorV1::new(&reads, &store, Ready);

        let outcome = coordinator.refresh(&context, &request).await;
        assert!(matches!(outcome, GitHubReviewRefreshOutcomeV1::Stored(_)));
        assert_eq!(reads.calls.load(Ordering::SeqCst), 2);
        let state = store.state.lock().unwrap().clone().expect("stored state");
        assert_eq!(state.attempt_receipts.len(), 1);
        assert_eq!(
            state.attempt_receipts[0].disposition,
            GitHubReviewRefreshAttemptDispositionV1::Agreed
        );
    }

    #[tokio::test]
    async fn cancelled_refresh_never_reads_or_writes_and_reports_cancellation() {
        let (context, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviewComments);
        let context = context.with_cancellation(
            CancellationContext::cancelled("cancel.github.refresh", UtcMicros(12)).unwrap(),
        );
        let reads = Reads::new(vec![read(complete_response(&request))]);
        let store = RefreshStore::default();
        let coordinator = GitHubReviewRefreshCoordinatorV1::new(&reads, &store, Ready);

        let outcome = coordinator.refresh(&context, &request).await;

        assert_eq!(outcome, GitHubReviewRefreshOutcomeV1::Cancelled);
        assert_eq!(reads.calls.load(Ordering::SeqCst), 0);
        assert_eq!(store.commits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn mixed_page_change_retries_once_then_quarantines_without_publication() {
        let (context, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviewComments);
        let first = complete_response(&request);
        let mut second = first.clone();
        second.ingress.merge_base_commit_id = CommitId::new("commit.github.merge-second").unwrap();
        let mut third = first.clone();
        third.ingress.merge_base_commit_id = CommitId::new("commit.github.merge-third").unwrap();
        let reads = Reads::new(vec![read(first), read(second), read(third)]);
        let store = RefreshStore::default();
        let coordinator = GitHubReviewRefreshCoordinatorV1::new(&reads, &store, Ready);

        assert_eq!(
            coordinator.refresh(&context, &request).await,
            GitHubReviewRefreshOutcomeV1::Stale
        );
        assert_eq!(reads.calls.load(Ordering::SeqCst), 3);
        let state = store
            .state
            .lock()
            .unwrap()
            .clone()
            .expect("attempt receipt");
        assert!(state.last_complete.is_none());
        assert_eq!(
            state.latest_attempt.ingress.outcome,
            GitHubReviewIngressProviderOutcomeV1::Partial
        );
        assert_eq!(
            state.attempt_receipts[0].disposition,
            GitHubReviewRefreshAttemptDispositionV1::Quarantined
        );
    }

    #[tokio::test]
    async fn mixed_page_retry_publishes_only_when_the_last_two_scans_agree() {
        let (context, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviewComments);
        let first = complete_response(&request);
        let mut stable = first.clone();
        stable.ingress.merge_base_commit_id = CommitId::new("commit.github.merge-stable").unwrap();
        let reads = Reads::new(vec![read(first), read(stable.clone()), read(stable)]);
        let store = RefreshStore::default();
        let coordinator = GitHubReviewRefreshCoordinatorV1::new(&reads, &store, Ready);

        assert!(matches!(
            coordinator.refresh(&context, &request).await,
            GitHubReviewRefreshOutcomeV1::Stored(_)
        ));
        assert_eq!(reads.calls.load(Ordering::SeqCst), 3);
        let state = store.state.lock().unwrap().clone().expect("stored state");
        assert_eq!(
            state.attempt_receipts[0].disposition,
            GitHubReviewRefreshAttemptDispositionV1::RetryAgreed
        );
    }

    #[test]
    fn not_modified_revalidates_the_last_complete_generation() {
        let (_, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviews);
        let previous =
            GitHubReviewRefreshStateV1::transition(&request, None, complete_response(&request))
                .unwrap();
        let mut not_modified = complete_response(&request);
        not_modified.ingress.outcome = GitHubReviewIngressProviderOutcomeV1::Stale;
        not_modified.ingress.coverage = GitHubReviewCoverageV1::Stale;
        not_modified.ingress.items.clear();
        not_modified.ingress.fetched_at = UtcMicros(12);
        not_modified.checkpoint.etag = Some(GitHubReviewEtagV1::new("W/\"revalidated\"").unwrap());

        let next = GitHubReviewRefreshStateV1::transition(&request, Some(&previous), not_modified)
            .unwrap();
        let complete = &next.last_complete.unwrap().response;
        assert_eq!(
            complete.ingress.outcome,
            GitHubReviewIngressProviderOutcomeV1::Complete
        );
        assert_eq!(complete.ingress.coverage, GitHubReviewCoverageV1::Complete);
        assert_eq!(complete.ingress.fetched_at, UtcMicros(12));
        assert_eq!(
            complete.checkpoint.etag.as_ref().unwrap().as_str(),
            "W/\"revalidated\""
        );
    }

    #[test]
    fn complete_pr_detail_scans_replace_partial_conversation_and_reviews() {
        for operation in [
            GitHubReviewReadOperationV1::RestListPullRequestConversationComments,
            GitHubReviewReadOperationV1::RestListPullRequestReviews,
        ] {
            let (_, request) = context_and_request(operation);
            let complete =
                GitHubReviewRefreshStateV1::transition(&request, None, complete_response(&request))
                    .unwrap();
            let mut observed = complete_response(&request);
            if operation == GitHubReviewReadOperationV1::RestListPullRequestConversationComments {
                observed
                    .ingress
                    .conversation
                    .push(GitHubConversationCommentV1 {
                        comment_id: GitHubReviewCommentIdV1::new("1").unwrap(),
                        author: None,
                        author_class: GitHubReviewAuthorClassV1::OtherObservedRole,
                        body: "A comment removed before the complete scan".to_owned(),
                        url: "https://github.com/owner/repository/pull/1#issuecomment-1".to_owned(),
                        created_at: "2026-10-26T12:00:00Z".to_owned(),
                        updated_at: "2026-10-26T12:00:00Z".to_owned(),
                    });
            } else {
                observed.ingress.reviews.push(GitHubPullRequestReviewV1 {
                    review_id: GitHubReviewIdV1::new("1").unwrap(),
                    state: GitHubReviewStateV1::Approved,
                    author: None,
                    body: None,
                    commit_id: Some(request.scope.head_commit_id.clone()),
                    submitted_at: None,
                    url: None,
                });
            }
            let partial = GitHubReviewRefreshStateV1::transition(
                &request,
                Some(&complete),
                quarantined_refresh_attempt(&request, observed).unwrap(),
            )
            .unwrap();
            let partial = GitHubReviewRefreshStateV1::transition(
                &request,
                Some(&partial),
                quarantined_refresh_attempt(&request, complete_response(&request)).unwrap(),
            )
            .unwrap();
            assert_eq!(
                partial.latest_attempt.ingress.conversation.len()
                    + partial.latest_attempt.ingress.reviews.len(),
                1,
                "an incomplete observation cannot establish deletion"
            );

            for outcome in [
                GitHubReviewIngressProviderOutcomeV1::Complete,
                GitHubReviewIngressProviderOutcomeV1::Stale,
            ] {
                let mut response = complete_response(&request);
                response.ingress.outcome = outcome;
                if outcome == GitHubReviewIngressProviderOutcomeV1::Stale {
                    response.ingress.coverage = GitHubReviewCoverageV1::Stale;
                }
                response.ingress.fetched_at = UtcMicros(12);
                let refreshed =
                    GitHubReviewRefreshStateV1::transition(&request, Some(&partial), response)
                        .unwrap();
                let latest = &refreshed.latest_attempt.ingress;
                assert_eq!(
                    latest.outcome,
                    GitHubReviewIngressProviderOutcomeV1::Complete
                );
                assert_eq!(latest.fetched_at, UtcMicros(12));
                assert!(latest.conversation.is_empty());
                assert!(latest.reviews.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn rest_get_restarts_at_page_one_and_forwards_rate_limit_without_any_write_operation() {
        let (context, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviews);
        let calls = Arc::new(NetworkCalls::default());
        let transport = GitHubReadOnlyRuntimeTransportV1::new(
            Checkpoints(Some(GitHubReviewReadCheckpointV1 {
                etag: Some(GitHubReviewEtagV1::new("W/\"cached\"").unwrap()),
                next_cursor: Some(GitHubReviewCursorV1::new("cursor.cached").unwrap()),
                rate_limit: Some(GitHubReviewRateLimitCheckpointV1 {
                    limit: 60,
                    remaining: 1,
                    reset_at: UtcMicros(90),
                }),
            })),
            Network {
                calls: Arc::clone(&calls),
                outcome: rate_limited_response(),
            },
            Decoder,
        );
        let outcome = transport
            .rest_get(
                &context,
                GitHubRestDescriptorV1 {
                    operation: request.operation,
                },
                &request,
            )
            .await;
        let GitHubReviewReadPortOutcomeV1::Read(response) = outcome else {
            panic!("rate-limit response should remain a typed read result");
        };
        assert_eq!(
            response.ingress.outcome,
            GitHubReviewIngressProviderOutcomeV1::RateLimited
        );
        assert_eq!(response.checkpoint.rate_limit.unwrap().remaining, 0);
        assert_eq!(calls.get.load(Ordering::SeqCst), 1);
        assert_eq!(calls.query.load(Ordering::SeqCst), 0);
        let outbound = calls.last_rest.lock().unwrap().clone().unwrap();
        assert!(outbound.resume.cursor.is_none());
        assert!(outbound.resume.etag.is_none());
    }

    #[tokio::test]
    async fn rest_refresh_scan_follows_bounded_pages_from_page_one() {
        let (context, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviewComments);
        let page = |next_cursor: Option<&str>| {
            GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                metadata: GitHubReadNetworkMetadataV1 {
                    status: GitHubReadNetworkStatusV1::Ok,
                    etag: None,
                    next_cursor: next_cursor
                        .map(|cursor| GitHubReviewCursorV1::new(cursor.to_owned()))
                        .transpose()
                        .unwrap(),
                    rate_limit: None,
                    retry_at: None,
                },
                body: Vec::new(),
            })
        };
        let network = PageNetwork {
            outcomes: Mutex::new(VecDeque::from([page(Some("rest-page:2")), page(None)])),
            cursors: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        };
        let transport = GitHubReadOnlyRuntimeTransportV1::new(Checkpoints(None), network, Decoder);

        let outcome = transport
            .rest_get(
                &context,
                GitHubRestDescriptorV1 {
                    operation: request.operation,
                },
                &request,
            )
            .await;
        let GitHubReviewReadPortOutcomeV1::Read(response) = outcome else {
            panic!("full scan should complete");
        };
        assert_eq!(
            response.ingress.outcome,
            GitHubReviewIngressProviderOutcomeV1::Complete
        );
        assert_eq!(transport.network.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            *transport.network.cursors.lock().unwrap(),
            vec![None, Some("rest-page:2".to_owned())]
        );
    }

    struct NoReviewAnchors;

    impl GitHubCanonicalReviewAnchorAuthorityV1 for NoReviewAnchors {
        fn resolve<'a>(
            &'a self,
            _request: &'a GitHubReviewReadRequestV1,
            _seed: &'a GitHubReviewAnchorSeedV1,
        ) -> FeedbackPortFuture<'a, Option<GitHubCanonicalReviewAnchorsV1>> {
            Box::pin(async { None })
        }
    }

    impl GitHubCurrentBranchRemapper for NoReviewAnchors {
        fn remap<'a>(
            &'a self,
            _context: &'a RequestContext,
            _current_scope: &'a FeedbackScopeV1,
            _original: &'a GitHubReviewImmutableAnchorV1,
        ) -> FeedbackPortFuture<'a, Option<GitHubReviewCurrentBranchRemapV1>> {
            panic!("review votes must not require inline anchor remapping");
        }
    }

    #[tokio::test]
    async fn pr_conversation_and_review_vote_scans_retain_every_page() {
        for operation in [
            GitHubReviewReadOperationV1::RestListPullRequestConversationComments,
            GitHubReviewReadOperationV1::RestListPullRequestReviews,
        ] {
            let (context, request) = context_and_request(operation);
            let page = |id: u64, next_cursor: Option<&str>| {
                let item = match operation {
                    GitHubReviewReadOperationV1::RestListPullRequestConversationComments => {
                        serde_json::json!({
                            "id": id, "body": format!("Discussion item {id}"),
                            "html_url": format!("https://github.com/ScriptedAlchemy/tracedecay/pull/421#issuecomment-{id}"),
                            "user": {"login": "reviewer", "type": "User"},
                            "created_at": "2026-10-26T12:00:00Z", "updated_at": "2026-10-26T12:00:00Z"
                        })
                    }
                    _ => serde_json::json!({
                        "id": id, "state": "APPROVED", "body": "Reviewed.",
                        "commit_id": request.scope.head_commit_id.as_str()
                    }),
                };
                GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::Ok,
                        etag: Some(GitHubReviewEtagV1::new(format!("W/\"page-{id}\"")).unwrap()),
                        next_cursor: next_cursor
                            .map(|cursor| GitHubReviewCursorV1::new(cursor).unwrap()),
                        rate_limit: None,
                        retry_at: None,
                    },
                    body: serde_json::to_vec(&vec![item]).unwrap(),
                })
            };
            let decoder = GitHubOfficialResponseDecoderV1::new(
                GitHubReviewProviderIdentityV1 {
                    provider: ProviderId::new("github").unwrap(),
                    repository_owner: "ScriptedAlchemy".to_owned(),
                    repository_name: "tracedecay".to_owned(),
                    pull_request_number: 421,
                    base_commit_id: CommitId::new("commit.github.base").unwrap(),
                    head_commit_id: request.scope.head_commit_id.clone(),
                    merge_base_commit_id: CommitId::new("commit.github.base").unwrap(),
                },
                NoReviewAnchors,
            )
            .unwrap();
            let network = PageNetwork {
                outcomes: Mutex::new(VecDeque::from([
                    page(1, Some("rest-page:2")),
                    page(2, None),
                ])),
                cursors: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            };
            let transport =
                GitHubReadOnlyRuntimeTransportV1::new(Checkpoints(None), network, decoder);
            let outcome = transport
                .rest_get(&context, GitHubRestDescriptorV1 { operation }, &request)
                .await;
            let GitHubReviewReadPortOutcomeV1::Read(response) = outcome else {
                panic!("whole PR detail scan must retain both pages");
            };
            assert_eq!(
                response.ingress.outcome,
                GitHubReviewIngressProviderOutcomeV1::Complete
            );
            assert!(response.ingress.items.is_empty());
            if operation == GitHubReviewReadOperationV1::RestListPullRequestConversationComments {
                assert_eq!(response.ingress.conversation.len(), 2);
                assert_eq!(response.ingress.conversation[0].comment_id.as_str(), "1");
                assert_eq!(response.ingress.conversation[1].comment_id.as_str(), "2");
            } else {
                assert_eq!(response.ingress.reviews.len(), 2);
                assert_eq!(response.ingress.reviews[0].review_id.as_str(), "1");
                assert_eq!(response.ingress.reviews[1].review_id.as_str(), "2");
            }
            assert_eq!(transport.network.calls.load(Ordering::SeqCst), 2);
            assert!(
                response.checkpoint.etag.is_none(),
                "a continuation-page ETag cannot revalidate the entire collection"
            );
        }
    }

    #[tokio::test]
    async fn review_scan_consensus_retains_partial_prose_and_collection_membership() {
        for withheld_id in [1, 2] {
            let (context, request) =
                context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviews);
            let page = |id: u64, next_cursor: Option<&str>, withhold: bool| {
                GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::Ok,
                        etag: Some(GitHubReviewEtagV1::new(format!("W/\"page-{id}\"")).unwrap()),
                        next_cursor: next_cursor
                            .map(|cursor| GitHubReviewCursorV1::new(cursor).unwrap()),
                        rate_limit: None,
                        retry_at: None,
                    },
                    body: serde_json::to_vec(&serde_json::json!([{
                        "id": id, "state": "APPROVED",
                        "body": if withhold && id == withheld_id {
                            "X-Vault_Passphrase: ordinary-value\nsee the second call below"
                        } else { "Reviewed." },
                        "commit_id": request.scope.head_commit_id.as_str()
                    }]))
                    .unwrap(),
                })
            };
            let decoder = GitHubOfficialResponseDecoderV1::new(
                GitHubReviewProviderIdentityV1 {
                    provider: ProviderId::new("github").unwrap(),
                    repository_owner: "ScriptedAlchemy".to_owned(),
                    repository_name: "tracedecay".to_owned(),
                    pull_request_number: 421,
                    base_commit_id: CommitId::new("commit.github.base").unwrap(),
                    head_commit_id: request.scope.head_commit_id.clone(),
                    merge_base_commit_id: CommitId::new("commit.github.base").unwrap(),
                },
                NoReviewAnchors,
            )
            .unwrap();
            let network = PageNetwork {
                outcomes: Mutex::new(VecDeque::from([
                    page(1, Some("rest-page:2"), false),
                    page(2, Some("rest-page:3"), false),
                    page(3, None, false),
                    page(1, Some("rest-page:2"), true),
                    page(2, None, true),
                    page(1, Some("rest-page:2"), true),
                    page(2, None, true),
                ])),
                cursors: Mutex::new(Vec::new()),
                calls: AtomicUsize::new(0),
            };
            let transport =
                GitHubReadOnlyRuntimeTransportV1::new(Checkpoints(None), network, decoder);
            let outcome = transport
                .rest_get(
                    &context,
                    GitHubRestDescriptorV1 {
                        operation: request.operation,
                    },
                    &request,
                )
                .await;
            let GitHubReviewReadPortOutcomeV1::Read(response) = outcome else {
                panic!("the initial complete review scan must retain all three pages");
            };
            assert_eq!(
                response.ingress.outcome,
                GitHubReviewIngressProviderOutcomeV1::Complete
            );
            assert_eq!(response.ingress.reviews.len(), 3);
            assert!(response.completed_collection_scan);
            let previous =
                GitHubReviewRefreshStateV1::transition(&request, None, *response).unwrap();
            let store = RefreshStore::default();
            *store.state.lock().unwrap() = Some(previous);
            let connector = GitHubReadOnlyConnector::new(
                GitHubReadOnlyDescriptorSetV1::new(vec![GitHubRestDescriptorV1 {
                    operation: request.operation,
                }])
                .unwrap(),
                transport,
                NoReviewAnchors,
            )
            .unwrap();
            let coordinator = GitHubReviewRefreshCoordinatorV1::new(connector, &store, Ready);
            let refreshed = coordinator.refresh(&context, &request).await;
            assert!(refreshed.has_usable_observation());
            let GitHubReviewRefreshOutcomeV1::Stored(receipt) = refreshed else {
                panic!("two equivalent partial-prose scans must establish collection membership");
            };
            let mut state = receipt.state;
            assert_eq!(
                state.attempt_receipts.last().unwrap().disposition,
                GitHubReviewRefreshAttemptDispositionV1::Agreed,
            );
            assert_eq!(
                state
                    .last_complete
                    .as_ref()
                    .unwrap()
                    .response
                    .ingress
                    .reviews
                    .len(),
                3
            );
            assert!(
                state
                    .last_partial_collection
                    .as_ref()
                    .unwrap()
                    .validate_for(&request)
            );
            let latest = &state.latest_attempt;
            assert_eq!(latest.ingress.coverage, GitHubReviewCoverageV1::Partial);
            assert_eq!(
                latest.ingress.outcome,
                GitHubReviewIngressProviderOutcomeV1::Partial
            );
            assert!(latest.completed_collection_scan);
            assert!(latest.checkpoint.next_cursor.is_none());
            assert!(latest.checkpoint.etag.is_none());
            assert_eq!(review_ids(latest), ["1", "2"]);
            for review in &latest.ingress.reviews {
                assert_eq!(review.state, GitHubReviewStateV1::Approved);
                if review.review_id.as_str() == withheld_id.to_string() {
                    assert!(review.body.is_none());
                } else {
                    assert_eq!(review.body.as_deref(), Some("Reviewed."));
                }
            }
            assert_eq!(store.commits.load(Ordering::SeqCst), 1);

            // A 304 cannot resurrect the older full-prose generation after a
            // newer collection without an aggregate ETag removed review 3.
            let mut stale = latest.clone();
            stale.completed_collection_scan = false;
            stale.ingress.outcome = GitHubReviewIngressProviderOutcomeV1::Stale;
            stale.ingress.coverage = GitHubReviewCoverageV1::Stale;
            stale.ingress.reviews.clear();
            stale.checkpoint.etag = Some(GitHubReviewEtagV1::new("W/\"older\"").unwrap());
            assert!(
                GitHubReviewRefreshStateV1::transition(&request, Some(&state), stale).is_none()
            );

            // The baseline proof is independent of the bounded attempt ring.
            // Each rate-limited attempt retains membership without relabeling
            // the partial collection as a complete prose generation.
            for _ in 0..=MAX_GITHUB_REFRESH_ATTEMPT_RECEIPTS_V1 {
                let mut limited = state.latest_attempt.clone();
                limited.completed_collection_scan = false;
                limited.ingress.outcome = GitHubReviewIngressProviderOutcomeV1::RateLimited;
                limited.ingress.coverage = GitHubReviewCoverageV1::Partial;
                limited.ingress.reviews.clear();
                limited.ingress.fetched_at.0 += 1;
                limited.checkpoint.rate_limit = Some(GitHubReviewRateLimitCheckpointV1 {
                    limit: 5_000,
                    remaining: 0,
                    reset_at: UtcMicros(limited.ingress.fetched_at.0 + 1_000),
                });
                let attempt = GitHubReviewRefreshAttemptReceiptV1 {
                    disposition: GitHubReviewRefreshAttemptDispositionV1::Terminal,
                    scan_digests: vec![github_review_scan_digest(&request, &limited).unwrap()],
                    observed_at: limited.ingress.fetched_at,
                };
                state = GitHubReviewRefreshStateV1::transition_with_receipt(
                    &request,
                    Some(&state),
                    limited,
                    Some(attempt),
                )
                .unwrap();
                assert_eq!(review_ids(&state.latest_attempt), ["1", "2"]);
            }
            assert!(state.attempt_receipts.iter().all(|receipt| {
                receipt.disposition == GitHubReviewRefreshAttemptDispositionV1::Terminal
            }));
            let encoded = serde_json::to_string(&state).unwrap();
            state = serde_json::from_str(&encoded).unwrap();
            assert!(state.validate_for(&request));
            assert!(
                state
                    .last_partial_collection
                    .as_ref()
                    .unwrap()
                    .validate_for(&request)
            );

            // An opaque continued page adds its observed ID to that baseline;
            // neither the removed ID nor a missing baseline vote may return.
            let mut continued = state.latest_attempt.clone();
            continued.ingress.outcome = GitHubReviewIngressProviderOutcomeV1::Partial;
            continued.ingress.fetched_at.0 += 1;
            continued.ingress.reviews.truncate(1);
            continued.ingress.reviews[0].review_id = GitHubReviewIdV1::new("4").unwrap();
            continued.checkpoint.rate_limit = None;
            continued.checkpoint.next_cursor =
                Some(GitHubReviewCursorV1::new("rest-page:2").unwrap());
            state =
                GitHubReviewRefreshStateV1::transition(&request, Some(&state), continued).unwrap();
            assert_eq!(review_ids(&state.latest_attempt), ["1", "2", "4"]);
            assert!(!state.latest_attempt.completed_collection_scan);
            assert!(state.last_partial_collection.is_some());

            let mut complete = state.latest_attempt.clone();
            complete.ingress.outcome = GitHubReviewIngressProviderOutcomeV1::Complete;
            complete.ingress.coverage = GitHubReviewCoverageV1::Complete;
            complete.ingress.fetched_at.0 += 1;
            complete.ingress.reviews.truncate(1);
            complete.checkpoint.next_cursor = None;
            state =
                GitHubReviewRefreshStateV1::transition(&request, Some(&state), complete).unwrap();
            assert!(state.last_partial_collection.is_none());
            assert_eq!(review_ids(&state.latest_attempt), ["1"]);
        }
    }

    fn review_ids(response: &GitHubReviewReadResponseV1) -> Vec<&str> {
        response
            .ingress
            .reviews
            .iter()
            .map(|review| review.review_id.as_str())
            .collect()
    }

    fn partial_review_collection(
        request: &GitHubReviewReadRequestV1,
        ids: &[&str],
        at: i64,
    ) -> GitHubReviewReadResponseV1 {
        let mut response = complete_response(request);
        response.completed_collection_scan = true;
        response.ingress.outcome = GitHubReviewIngressProviderOutcomeV1::Partial;
        response.ingress.coverage = GitHubReviewCoverageV1::Partial;
        response.ingress.fetched_at = UtcMicros(at);
        response.ingress.reviews = ids
            .iter()
            .map(|id| GitHubPullRequestReviewV1 {
                review_id: GitHubReviewIdV1::new(*id).unwrap(),
                state: GitHubReviewStateV1::Approved,
                author: None,
                body: None,
                commit_id: Some(request.scope.head_commit_id.clone()),
                submitted_at: None,
                url: None,
            })
            .collect();
        response
    }

    #[tokio::test]
    async fn partial_collection_disagreement_preserves_retained_membership() {
        let (context, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviews);
        let previous = partial_review_collection(&request, &["1", "2", "3"], 10);
        let previous = GitHubReviewRefreshStateV1::transition(&request, None, previous).unwrap();
        assert!(!previous.latest_attempt.completed_collection_scan);
        let store = RefreshStore::default();
        *store.state.lock().unwrap() = Some(previous);
        let reads = Reads::new(vec![
            read(partial_review_collection(&request, &["1", "2"], 11)),
            read(partial_review_collection(&request, &["1"], 12)),
            read(partial_review_collection(&request, &["2"], 13)),
        ]);
        let coordinator = GitHubReviewRefreshCoordinatorV1::new(&reads, &store, Ready);
        assert_eq!(
            coordinator.refresh(&context, &request).await,
            GitHubReviewRefreshOutcomeV1::Stale
        );
        assert_eq!(reads.calls.load(Ordering::SeqCst), 3);
        let state = store.state.lock().unwrap().clone().unwrap();
        assert_eq!(review_ids(&state.latest_attempt), ["1", "2", "3"]);
        assert!(state.last_partial_collection.is_none());
        assert!(!state.latest_attempt.completed_collection_scan);

        let reads = Reads::new(vec![
            read(partial_review_collection(&request, &["1"], 14)),
            read(partial_review_collection(&request, &["1", "2"], 15)),
            read(partial_review_collection(&request, &["1", "2"], 16)),
        ]);
        let coordinator = GitHubReviewRefreshCoordinatorV1::new(&reads, &store, Ready);
        assert!(
            coordinator
                .refresh(&context, &request)
                .await
                .has_usable_observation()
        );
        assert_eq!(reads.calls.load(Ordering::SeqCst), 3);
        let state = store.state.lock().unwrap().clone().unwrap();
        assert_eq!(review_ids(&state.latest_attempt), ["1", "2"]);
        let baseline = state.last_partial_collection.clone().unwrap();
        assert_eq!(
            baseline.receipt.disposition,
            GitHubReviewRefreshAttemptDispositionV1::RetryAgreed
        );

        let reads = Reads::new(vec![
            read(partial_review_collection(&request, &["1"], 16)),
            read(partial_review_collection(&request, &["2"], 17)),
            read(partial_review_collection(&request, &["4"], 18)),
        ]);
        let coordinator = GitHubReviewRefreshCoordinatorV1::new(&reads, &store, Ready);
        assert_eq!(
            coordinator.refresh(&context, &request).await,
            GitHubReviewRefreshOutcomeV1::Stale
        );
        assert_eq!(reads.calls.load(Ordering::SeqCst), 3);
        let state = store.state.lock().unwrap().clone().unwrap();
        assert_eq!(state.last_partial_collection.as_ref(), Some(&baseline));
        assert_eq!(review_ids(&state.latest_attempt), ["1", "2", "4"]);
        assert!(state.validate_for(&request));
    }

    #[test]
    fn partial_collection_authority_is_bound_to_the_current_acquisition() {
        let (_, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviews);
        let previous = partial_review_collection(&request, &["1", "2", "3"], 10);
        let previous = GitHubReviewRefreshStateV1::transition(&request, None, previous).unwrap();
        let scan = partial_review_collection(&request, &["1", "2"], 11);
        let digest = github_review_scan_digest(&request, &scan).unwrap();
        let agreed = GitHubReviewRefreshAttemptReceiptV1 {
            disposition: GitHubReviewRefreshAttemptDispositionV1::Agreed,
            scan_digests: vec![digest.clone(), digest.clone()],
            observed_at: scan.ingress.fetched_at,
        };
        let mut wrong_time = agreed.clone();
        wrong_time.observed_at = UtcMicros(10);
        let mut wrong_content = agreed.clone();
        let other =
            github_review_scan_digest(&request, &partial_review_collection(&request, &["1"], 11))
                .unwrap();
        wrong_content.scan_digests = vec![other.clone(), other];
        let terminal = GitHubReviewRefreshAttemptReceiptV1 {
            disposition: GitHubReviewRefreshAttemptDispositionV1::Terminal,
            scan_digests: vec![digest],
            observed_at: scan.ingress.fetched_at,
        };
        for receipt in [None, Some(terminal), Some(wrong_time), Some(wrong_content)] {
            let state = GitHubReviewRefreshStateV1::transition_with_receipt(
                &request,
                Some(&previous),
                scan.clone(),
                receipt,
            )
            .unwrap();
            assert_eq!(review_ids(&state.latest_attempt), ["1", "2", "3"]);
            assert!(state.last_partial_collection.is_none());
            assert!(!state.latest_attempt.completed_collection_scan);
        }
        let state = GitHubReviewRefreshStateV1::transition_with_receipt(
            &request,
            Some(&previous),
            scan,
            Some(agreed),
        )
        .unwrap();
        assert_eq!(review_ids(&state.latest_attempt), ["1", "2"]);
        let mut tampered = state.clone();
        tampered
            .last_partial_collection
            .as_mut()
            .unwrap()
            .receipt
            .observed_at = UtcMicros(10);
        assert!(!tampered.validate_for(&request));
        let mut wrong_scope = request.clone();
        wrong_scope.pull_request_id = GitHubPullRequestIdV1::new("another-pr").unwrap();
        assert!(
            !state
                .last_partial_collection
                .as_ref()
                .unwrap()
                .validate_for(&wrong_scope)
        );
    }

    #[test]
    fn absent_collection_provenance_preserves_stored_revision_and_wire_bytes() {
        let (_, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviews);
        let response = complete_response(&request);
        let encoded_response = serde_json::to_value(&response).unwrap();
        assert!(encoded_response.get("completed_collection_scan").is_none());
        let state = GitHubReviewRefreshStateV1::transition(&request, None, response).unwrap();
        let expected = canonical_sha256(&(
            GITHUB_REVIEW_REFRESH_STATE_DOMAIN_V1,
            state
                .last_complete
                .as_ref()
                .map(|generation| &generation.response),
            &state.latest_attempt,
        ))
        .unwrap();
        assert_eq!(state.revision, expected);
        let encoded_state = serde_json::to_value(&state).unwrap();
        assert!(encoded_state.get("last_partial_collection").is_none());
        let decoded: GitHubReviewRefreshStateV1 = serde_json::from_value(encoded_state).unwrap();
        assert_eq!(decoded, state);
        assert!(decoded.validate_for(&request));
    }

    #[tokio::test]
    async fn corrupt_resume_and_graphql_routing_fail_closed_before_any_get() {
        let (context, request) =
            context_and_request(GitHubReviewReadOperationV1::RestGetPullRequest);
        let calls = Arc::new(NetworkCalls::default());
        let transport = GitHubReadOnlyRuntimeTransportV1::new(
            Checkpoints(Some(GitHubReviewReadCheckpointV1 {
                etag: None,
                next_cursor: None,
                rate_limit: Some(GitHubReviewRateLimitCheckpointV1 {
                    limit: 0,
                    remaining: 0,
                    reset_at: UtcMicros(1),
                }),
            })),
            Network {
                calls: Arc::clone(&calls),
                outcome: rate_limited_response(),
            },
            Decoder,
        );
        assert_eq!(
            transport
                .rest_get(
                    &context,
                    GitHubRestDescriptorV1 {
                        operation: request.operation,
                    },
                    &request,
                )
                .await,
            GitHubReviewReadPortOutcomeV1::Unavailable
        );
        assert_eq!(calls.get.load(Ordering::SeqCst), 0);
        assert_eq!(calls.query.load(Ordering::SeqCst), 0);

        let (context, request) =
            context_and_request(GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads);
        let calls = Arc::new(NetworkCalls::default());
        let transport = GitHubReadOnlyRuntimeTransportV1::new(
            Checkpoints(None),
            Network {
                calls: Arc::clone(&calls),
                outcome: GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::Ok,
                        etag: None,
                        next_cursor: None,
                        rate_limit: None,
                        retry_at: None,
                    },
                    body: Vec::new(),
                }),
            },
            Decoder,
        );
        assert!(matches!(
            transport.graphql_query(&context, &request).await,
            GitHubReviewReadPortOutcomeV1::Read(_)
        ));
        assert_eq!(calls.get.load(Ordering::SeqCst), 0);
        assert_eq!(calls.query.load(Ordering::SeqCst), 1);
        assert!(calls.last_query.lock().unwrap().is_some());
    }
}
