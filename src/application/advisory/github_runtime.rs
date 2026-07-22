//! Concrete, injected runtime transport for the closed GitHub review-read port.
//!
//! The concrete owner admits exact scope and source access before invoking a
//! client that exposes only fixed REST GETs and one static GraphQL query.

mod anchors;
mod decoder;
mod dto;
mod network;
mod owner;
mod source;
mod store;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    FeedbackPortFuture, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
    GitHubReviewReadPort, GitHubReviewReadPortOutcomeV1, GitHubReviewReadRequestV1,
    GitHubReviewReadResponseV1,
};
use tracedecay_domain::feedback::{
    GitHubPullRequestIdV1, GitHubReviewCursorV1, GitHubReviewEtagV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1,
    GitHubReviewRateLimitCheckpointV1, GitHubReviewReadCheckpointV1,
};
use tracedecay_domain::{ManifestDigest, canonical_sha256};

use super::{GitHubReadOnlyTransport, GitHubRestDescriptorV1, context_allows_feedback_operation};

pub use anchors::{
    ProjectGitHubAnchorAuthorityV1, ProjectGitHubRegistrarAuthoritiesV1,
    github_anchor_authorities_arc_v1, github_anchor_authorities_v1,
};
pub use decoder::{
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCanonicalReviewAnchorsV1,
    GitHubOfficialResponseDecoderV1, GitHubReviewAnchorSeedV1, GitHubReviewProviderIdentityV1,
};
pub use dto::{
    GitHubActionsCheckRunOutputV1, GitHubActionsCheckRunV1, GitHubActionsCheckSuiteRefV1,
    GitHubActionsConclusionV1, GitHubActionsPullRequestRefV1, GitHubActionsStatusV1,
    GitHubActionsWorkflowJobV1, GitHubActionsWorkflowRunV1, GitHubActionsWorkflowStepV1,
    GitHubCheckAnnotationLevelV1, GitHubCheckAnnotationV1, GitHubRetainedResponseV1,
};
pub(crate) use dto::{GraphQlResponseV1, RestPullRequestV1, RestReviewCommentV1, RestReviewV1};
pub use network::{
    GITHUB_REVIEW_THREADS_QUERY_V1, GitHubCiTransportOutcomeV1, GitHubHttpReadConfigV1,
    GitHubReadOnlyClientV1, GitHubReadOnlyCredentialV1, GitHubReadPermissionV1,
    GitHubRepositoryTargetV1,
};
pub use owner::{
    GitHubProviderLifecycleV1, GitHubReviewRuntimeOwnerBuildErrorV1,
    GitHubReviewRuntimeOwnerConfigV1, GitHubReviewRuntimeOwnerV1,
    build_github_review_runtime_owner_v1,
};
pub use store::ProjectGitHubReviewStoreV1;

/// Raw GitHub response bytes are transient parser input only. They are never
/// put into a checkpoint, an ingress result, or this transport's receipt.
pub const MAX_GITHUB_READ_RESPONSE_BYTES_V1: usize = 1024 * 1024;

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
    pub scope: tracedecay_domain::feedback::FeedbackScopeV1,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub resume: GitHubReadResumeV1,
}

/// The only GraphQL-shaped request emitted by the runtime. Query text is not
/// representable here; the concrete client owns one compile-time document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubGraphQlReadRequestV1 {
    pub scope: tracedecay_domain::feedback::FeedbackScopeV1,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub resume: GitHubReadResumeV1,
}

/// Read response metadata retained by the domain checkpoint. It deliberately
/// does not expose arbitrary headers, a status code, or a redirect location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubReadNetworkMetadataV1 {
    pub status: GitHubReadNetworkStatusV1,
    pub etag: Option<GitHubReviewEtagV1>,
    pub next_cursor: Option<GitHubReviewCursorV1>,
    pub rate_limit: Option<GitHubReviewRateLimitCheckpointV1>,
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
            && (self.status != GitHubReadNetworkStatusV1::RateLimited || self.rate_limit.is_some())
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

/// Parser/normalizer for bounded transient response bytes. It receives no
/// credentials and returns only the source-owned, anchor-based domain result.
pub trait GitHubReadResponseDecoderV1 {
    fn decode<'a>(
        &'a self,
        request: &'a GitHubReviewReadRequestV1,
        metadata: &'a GitHubReadNetworkMetadataV1,
        body: &'a [u8],
    ) -> FeedbackPortFuture<'a, Option<GitHubReviewIngressResultV1>>;
}

/// Concrete runtime implementation of the existing read-only transport port.
/// All network, checkpoint, and response-decoding authorities are injected;
/// daemon wiring chooses their implementations later.
pub struct GitHubReadOnlyRuntimeTransportV1<C, N, D> {
    checkpoints: C,
    network: N,
    decoder: D,
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
    ) -> GitHubReviewReadPortOutcomeV1 {
        let response = match outcome {
            GitHubReadNetworkOutcomeV1::Response(response) => response,
            GitHubReadNetworkOutcomeV1::Denied => return GitHubReviewReadPortOutcomeV1::Denied,
            GitHubReadNetworkOutcomeV1::Unavailable => {
                return GitHubReviewReadPortOutcomeV1::Unavailable;
            }
        };
        if !response.validate() {
            return GitHubReviewReadPortOutcomeV1::Unavailable;
        }
        let Some(ingress) = self
            .decoder
            .decode(request, &response.metadata, &response.body)
            .await
        else {
            return GitHubReviewReadPortOutcomeV1::Unavailable;
        };
        if ingress.operation != request.operation
            || ingress.scope != request.scope
            || ingress.pull_request_id != request.pull_request_id
            || !network_status_matches(response.metadata.status, ingress.outcome)
        {
            return GitHubReviewReadPortOutcomeV1::Unavailable;
        }
        let checkpoint = GitHubReviewReadCheckpointV1 {
            etag: response.metadata.etag.or(resume.etag),
            next_cursor: if ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Complete {
                response.metadata.next_cursor
            } else {
                response.metadata.next_cursor.or(resume.cursor)
            },
            rate_limit: response.metadata.rate_limit.or(resume.rate_limit),
        };
        let response = GitHubReviewReadResponseV1 {
            ingress,
            checkpoint,
        };
        if response.validate_for(request).is_ok() {
            GitHubReviewReadPortOutcomeV1::Read(Box::new(response))
        } else {
            GitHubReviewReadPortOutcomeV1::Unavailable
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
            let outbound = GitHubRestReadRequestV1 {
                descriptor,
                scope: request.scope.clone(),
                pull_request_id: request.pull_request_id.clone(),
                resume: resume.clone(),
            };
            let outcome = self.network.get(context, &outbound).await;
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReviewReadPortOutcomeV1::Denied;
            }
            self.decode_outcome(request, resume, outcome).await
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
            let outbound = GitHubGraphQlReadRequestV1 {
                scope: request.scope.clone(),
                pull_request_id: request.pull_request_id.clone(),
                resume: resume.clone(),
            };
            let outcome = self.network.query(context, &outbound).await;
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReviewReadPortOutcomeV1::Denied;
            }
            self.decode_outcome(request, resume, outcome).await
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

const GITHUB_REVIEW_REFRESH_STATE_DOMAIN_V1: &str = "tracedecay.pr13.github.refresh-state.v1";

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
}

impl GitHubReviewRefreshStateV1 {
    pub fn transition(
        request: &GitHubReviewReadRequestV1,
        previous: Option<&Self>,
        latest_attempt: GitHubReviewReadResponseV1,
    ) -> Option<Self> {
        if latest_attempt.validate_for(request).is_err()
            || previous.is_some_and(|state| !state.validate_for(request))
        {
            return None;
        }
        let latest_attempt = normalize_refresh_attempt(request, previous, latest_attempt)?;
        let last_complete =
            if latest_attempt.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Complete {
                Some(GitHubReviewCompleteGenerationV1::from_response(
                    request,
                    latest_attempt.clone(),
                )?)
            } else {
                previous.and_then(|state| state.last_complete.clone())
            };
        let revision = canonical_sha256(&(
            GITHUB_REVIEW_REFRESH_STATE_DOMAIN_V1,
            last_complete
                .as_ref()
                .map(|generation| &generation.response),
            &latest_attempt,
        ))
        .ok()?;
        let state = Self {
            revision,
            last_complete,
            latest_attempt,
        };
        state.validate_for(request).then_some(state)
    }

    pub fn validate_for(&self, request: &GitHubReviewReadRequestV1) -> bool {
        if self.latest_attempt.validate_for(request).is_err()
            || self
                .last_complete
                .as_ref()
                .is_some_and(|generation| !generation.validate_for(request))
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
        canonical_sha256(&(
            GITHUB_REVIEW_REFRESH_STATE_DOMAIN_V1,
            self.last_complete
                .as_ref()
                .map(|generation| &generation.response),
            &self.latest_attempt,
        ))
        .is_ok_and(|expected| expected == self.revision)
    }
}

fn normalize_refresh_attempt(
    request: &GitHubReviewReadRequestV1,
    previous: Option<&GitHubReviewRefreshStateV1>,
    mut latest: GitHubReviewReadResponseV1,
) -> Option<GitHubReviewReadResponseV1> {
    let mut items = BTreeMap::new();
    if let Some(previous_partial) = previous
        .map(|state| &state.latest_attempt)
        .filter(|response| {
            response.ingress.outcome == GitHubReviewIngressProviderOutcomeV1::Partial
                && response.ingress.operation == latest.ingress.operation
        })
    {
        for mut item in previous_partial.ingress.items.clone() {
            item.provider_outcome = latest.ingress.outcome;
            items.insert(item.comment_id.as_str().to_owned(), item);
        }
    }
    for item in latest.ingress.items.drain(..) {
        items.insert(item.comment_id.as_str().to_owned(), item);
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
                        && current.body_digest != prior.body_digest =>
                {
                    current.lifecycle =
                        tracedecay_domain::feedback::GitHubReviewLifecycleV1::Edited;
                }
                Some(_) => {}
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
    latest.validate_for(request).is_ok().then_some(latest)
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
    Denied,
    Stale,
    Unavailable,
}

/// One explicit, non-repeating refresh. A compare conflict is surfaced as
/// stale rather than retried, so this coordinator cannot become a polling or
/// autonomous ingestion loop.
pub struct GitHubReviewRefreshCoordinatorV1<P, S> {
    port: P,
    store: S,
}

impl<P, S> GitHubReviewRefreshCoordinatorV1<P, S> {
    pub fn new(port: P, store: S) -> Self {
        Self { port, store }
    }
}

impl<P, S> GitHubReviewRefreshCoordinatorV1<P, S>
where
    P: GitHubReviewReadPort + Sync,
    S: GitHubReviewAtomicRefreshStoreV1 + Sync,
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
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReviewRefreshOutcomeV1::Denied;
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
                    if !context_allows_feedback_operation(
                        context,
                        &request.scope,
                        GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                        GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
                    ) {
                        return GitHubReviewRefreshOutcomeV1::Denied;
                    }
                    return GitHubReviewRefreshOutcomeV1::Unavailable;
                }
            };
            let read = self.port.read(context, request).await;
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReviewRefreshOutcomeV1::Denied;
            }
            let GitHubReviewReadPortOutcomeV1::Read(response) = read else {
                return match read {
                    GitHubReviewReadPortOutcomeV1::Denied => GitHubReviewRefreshOutcomeV1::Denied,
                    GitHubReviewReadPortOutcomeV1::Unavailable => {
                        GitHubReviewRefreshOutcomeV1::Unavailable
                    }
                    GitHubReviewReadPortOutcomeV1::Read(_) => unreachable!(),
                };
            };
            let Some(next) =
                GitHubReviewRefreshStateV1::transition(request, previous.as_deref(), *response)
            else {
                return GitHubReviewRefreshOutcomeV1::Unavailable;
            };
            let expected = previous.as_ref().map(|state| &state.revision);
            let outcome = self
                .store
                .compare_and_record(context, request, expected, &next)
                .await;
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
                GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
            ) {
                return GitHubReviewRefreshOutcomeV1::Denied;
            }
            match outcome {
                GitHubReviewRefreshStoreCommitOutcomeV1::Recorded
                | GitHubReviewRefreshStoreCommitOutcomeV1::Duplicate => {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::db::{Database, DatabaseAuthority};
    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::feedback::{
        FeedbackScopeV1, GitHubReviewCoverageV1, GitHubReviewReadOperationV1,
    };
    use tracedecay_domain::{
        ActorId, CommitId, ManifestDigest, ProjectId, ProviderId, RefId, RepositoryId, UtcMicros,
        WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

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

    struct Decoder;

    impl GitHubReadResponseDecoderV1 for Decoder {
        fn decode<'a>(
            &'a self,
            request: &'a GitHubReviewReadRequestV1,
            metadata: &'a GitHubReadNetworkMetadataV1,
            _body: &'a [u8],
        ) -> FeedbackPortFuture<'a, Option<GitHubReviewIngressResultV1>> {
            Box::pin(async move {
                let (outcome, coverage) = match metadata.status {
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
                Some(GitHubReviewIngressResultV1 {
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
                    fetched_at: UtcMicros(10),
                })
            })
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
            UtcMicros(1_000),
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
            Deadline::new(UtcMicros(900)).unwrap(),
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
            },
            body: Vec::new(),
        })
    }

    fn complete_response(request: &GitHubReviewReadRequestV1) -> GitHubReviewReadResponseV1 {
        GitHubReviewReadResponseV1 {
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
                fetched_at: UtcMicros(11),
            },
            checkpoint: GitHubReviewReadCheckpointV1 {
                etag: None,
                next_cursor: None,
                rate_limit: None,
            },
        }
    }

    #[tokio::test]
    async fn rest_get_forwards_resume_and_rate_limit_without_any_write_operation() {
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
        assert_eq!(outbound.resume.cursor.unwrap().as_str(), "cursor.cached");
        assert_eq!(outbound.resume.etag.unwrap().as_str(), "W/\"cached\"");
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

    #[tokio::test]
    async fn project_store_restarts_and_replays_the_github_source_commit() {
        let (context, request) =
            context_and_request(GitHubReviewReadOperationV1::RestListPullRequestReviewComments);
        let next =
            GitHubReviewRefreshStateV1::transition(&request, None, complete_response(&request))
                .expect("complete GitHub response creates a refresh state");
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("github-review.db");
        let authority = DatabaseAuthority::acquire_test(&path, "github-source-restart").unwrap();
        let (database, _) = Database::initialize(&path, &authority).await.unwrap();
        let store =
            ProjectGitHubReviewStoreV1::new(database.clone(), request.scope.clone()).unwrap();

        assert_eq!(
            store
                .compare_and_record(&context, &request, None, &next)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Recorded
        );
        assert!(store.source_state_for_test(&request).await.is_some());
        drop(store);
        database.close();

        let (database, _) = Database::open(&path, &authority).await.unwrap();
        let store = ProjectGitHubReviewStoreV1::new(database, request.scope.clone()).unwrap();
        assert!(store.source_state_for_test(&request).await.is_some());
        assert_eq!(
            store
                .compare_and_record(&context, &request, Some(&next.revision), &next)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Duplicate
        );
    }
}
