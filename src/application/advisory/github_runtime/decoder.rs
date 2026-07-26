use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_application::feedback::{FeedbackPortFuture, GitHubReviewReadRequestV1};
use tracedecay_domain::feedback::{
    GitHubReviewAuthorClassV1, GitHubReviewCommentIdV1, GitHubReviewCoverageV1,
    GitHubReviewCurrentBranchRemapV1, GitHubReviewIdV1, GitHubReviewImmutableAnchorV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1, GitHubReviewItemV1,
    GitHubReviewLifecycleV1, GitHubReviewReadOperationV1, GitHubReviewRemapStateV1,
    GitHubReviewStateV1, GitHubReviewThreadIdV1,
};
use tracedecay_domain::{CommitId, ManifestDigest, ProviderId, RetrievalAnchorId, UtcMicros};

use super::dto::{
    GraphQlResponseV1, GraphQlReviewCommentV1, GraphQlReviewThreadV1, RestPullRequestV1,
    RestReviewCommentV1, RestReviewV1,
};
use super::{GitHubReadNetworkMetadataV1, GitHubReadNetworkStatusV1, GitHubReadResponseDecoderV1};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubReviewProviderIdentityV1 {
    pub provider: ProviderId,
    pub repository_owner: String,
    pub repository_name: String,
    pub pull_request_number: u64,
    pub base_commit_id: CommitId,
    pub head_commit_id: CommitId,
    pub merge_base_commit_id: CommitId,
}

impl GitHubReviewProviderIdentityV1 {
    pub fn validate(&self) -> bool {
        self.provider.validate().is_ok()
            && valid_repository_segment(&self.repository_owner)
            && valid_repository_segment(&self.repository_name)
            && self.pull_request_number > 0
            && self.base_commit_id.validate().is_ok()
            && self.head_commit_id.validate().is_ok()
            && self.merge_base_commit_id.validate().is_ok()
    }

    fn admits_review_url(&self, value: &str) -> bool {
        let Ok(url) = url::Url::parse(value) else {
            return false;
        };
        let mut segments = match url.path_segments() {
            Some(segments) => segments,
            None => return false,
        };
        let pull_request_number = self.pull_request_number.to_string();
        url.scheme() == "https"
            && url.host_str() == Some("github.com")
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && segments.next() == Some(self.repository_owner.as_str())
            && segments.next() == Some(self.repository_name.as_str())
            && segments.next() == Some("pull")
            && segments.next() == Some(pull_request_number.as_str())
            && segments.next().is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubCanonicalReviewAnchorsV1 {
    pub original: GitHubReviewImmutableAnchorV1,
    pub initial_remap: GitHubReviewCurrentBranchRemapV1,
    pub author_anchor: RetrievalAnchorId,
    pub body_anchor: RetrievalAnchorId,
    pub safe_url_anchor: Option<RetrievalAnchorId>,
}

impl GitHubCanonicalReviewAnchorsV1 {
    fn validate(&self) -> bool {
        self.original.validate().is_ok()
            && self.initial_remap.validate().is_ok()
            && self.initial_remap.original == self.original
            && self.author_anchor.validate().is_ok()
            && self.body_anchor.validate().is_ok()
            && self
                .safe_url_anchor
                .as_ref()
                .is_none_or(|anchor| anchor.validate().is_ok())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubReviewAnchorSeedV1 {
    pub comment_id: GitHubReviewCommentIdV1,
    pub author_node_id: String,
    pub body_digest: ManifestDigest,
    pub safe_url: String,
    pub path: String,
    pub original_commit_id: CommitId,
    pub observed_commit_id: CommitId,
    pub original_start_line: Option<u64>,
    pub original_line: Option<u64>,
    pub current_start_line: Option<u64>,
    pub current_line: Option<u64>,
}

pub trait GitHubCanonicalReviewAnchorAuthorityV1 {
    fn resolve<'a>(
        &'a self,
        request: &'a GitHubReviewReadRequestV1,
        seed: &'a GitHubReviewAnchorSeedV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubCanonicalReviewAnchorsV1>>;
}

pub struct GitHubOfficialResponseDecoderV1<A> {
    identity: GitHubReviewProviderIdentityV1,
    anchors: A,
}

impl<A> GitHubOfficialResponseDecoderV1<A> {
    pub fn new(identity: GitHubReviewProviderIdentityV1, anchors: A) -> Option<Self> {
        identity.validate().then_some(Self { identity, anchors })
    }
}

impl<A> GitHubReadResponseDecoderV1 for GitHubOfficialResponseDecoderV1<A>
where
    A: GitHubCanonicalReviewAnchorAuthorityV1 + Sync,
{
    fn decode<'a>(
        &'a self,
        request: &'a GitHubReviewReadRequestV1,
        metadata: &'a GitHubReadNetworkMetadataV1,
        body: &'a [u8],
    ) -> FeedbackPortFuture<'a, Option<GitHubReviewIngressResultV1>> {
        Box::pin(async move {
            if request.validate().is_err()
                || request.scope.head_commit_id != self.identity.head_commit_id
            {
                return None;
            }
            let (outcome, coverage) =
                provider_state(metadata.status, metadata.next_cursor.is_some());
            if metadata.status != GitHubReadNetworkStatusV1::Ok {
                return self.ingress(request, outcome, coverage, Vec::new(), now_micros()?);
            }
            let fetched_at = now_micros()?;
            let items = match request.operation {
                GitHubReviewReadOperationV1::RestGetPullRequest => {
                    self.decode_pull_request(request, body)?;
                    Vec::new()
                }
                GitHubReviewReadOperationV1::RestListPullRequestReviews => {
                    self.decode_reviews(body)?;
                    Vec::new()
                }
                GitHubReviewReadOperationV1::RestListPullRequestReviewComments => {
                    self.decode_rest_comments(request, body, outcome, fetched_at)
                        .await?
                }
                GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads => {
                    self.decode_graphql_threads(request, body, outcome, fetched_at)
                        .await?
                }
            };
            self.ingress(request, outcome, coverage, items, fetched_at)
        })
    }
}

impl<A> GitHubOfficialResponseDecoderV1<A>
where
    A: GitHubCanonicalReviewAnchorAuthorityV1,
{
    fn ingress(
        &self,
        request: &GitHubReviewReadRequestV1,
        outcome: GitHubReviewIngressProviderOutcomeV1,
        coverage: GitHubReviewCoverageV1,
        items: Vec<GitHubReviewItemV1>,
        fetched_at: UtcMicros,
    ) -> Option<GitHubReviewIngressResultV1> {
        let ingress = GitHubReviewIngressResultV1 {
            provider: self.identity.provider.clone(),
            scope: request.scope.clone(),
            pull_request_id: request.pull_request_id.clone(),
            provider_base_commit_id: self.identity.base_commit_id.clone(),
            provider_head_commit_id: self.identity.head_commit_id.clone(),
            merge_base_commit_id: self.identity.merge_base_commit_id.clone(),
            operation: request.operation,
            outcome,
            coverage,
            items,
            fetched_at,
        };
        ingress.validate().ok()?;
        Some(ingress)
    }

    fn decode_pull_request(&self, request: &GitHubReviewReadRequestV1, body: &[u8]) -> Option<()> {
        let response = serde_json::from_slice::<RestPullRequestV1>(body).ok()?;
        (response.id.to_string() == request.pull_request_id.as_str()
            && response.number == self.identity.pull_request_number
            && response.base.sha == self.identity.base_commit_id.as_str()
            && response.head.sha == self.identity.head_commit_id.as_str())
        .then_some(())
    }

    fn decode_reviews(&self, body: &[u8]) -> Option<()> {
        let reviews = serde_json::from_slice::<Vec<RestReviewV1>>(body).ok()?;
        let mut review_ids = BTreeSet::new();
        reviews
            .iter()
            .all(|review| {
                review.id > 0
                    && review_ids.insert(review.id)
                    && review
                        .node_id
                        .as_ref()
                        .is_none_or(|value| !value.is_empty())
                    && review
                        .state
                        .as_deref()
                        .is_none_or(|state| review_state(state).is_some())
            })
            .then_some(())
    }

    async fn decode_rest_comments(
        &self,
        request: &GitHubReviewReadRequestV1,
        body: &[u8],
        outcome: GitHubReviewIngressProviderOutcomeV1,
        observed_at: UtcMicros,
    ) -> Option<Vec<GitHubReviewItemV1>> {
        let comments = serde_json::from_slice::<Vec<RestReviewCommentV1>>(body).ok()?;
        let mut items = Vec::with_capacity(comments.len());
        let mut comment_ids = BTreeSet::new();
        for comment in comments {
            if !comment_ids.insert(comment.id) {
                return None;
            }
            items.push(
                self.rest_comment_item(request, comment, outcome, observed_at)
                    .await?,
            );
        }
        Some(items)
    }

    async fn rest_comment_item(
        &self,
        request: &GitHubReviewReadRequestV1,
        comment: RestReviewCommentV1,
        outcome: GitHubReviewIngressProviderOutcomeV1,
        observed_at: UtcMicros,
    ) -> Option<GitHubReviewItemV1> {
        let provider_lifecycle = rest_lifecycle(&comment);
        let comment_id = GitHubReviewCommentIdV1::new(comment.id.to_string()).ok()?;
        let review_id = comment
            .pull_request_review_id
            .map(|id| GitHubReviewIdV1::new(id.to_string()))
            .transpose()
            .ok()?;
        let original_commit_id = CommitId::new(comment.original_commit_id).ok()?;
        let observed_commit_id = CommitId::new(comment.commit_id).ok()?;
        let body_digest = body_digest(comment.body.as_deref()?)?;
        let version_digest = review_version_digest(
            &comment_id,
            &comment.updated_at,
            &body_digest,
            &observed_commit_id,
        )?;
        self.identity
            .admits_review_url(&comment.html_url)
            .then_some(())?;
        let user = comment.user.as_ref()?;
        let user_node_id = user.node_id.clone();
        let user_kind = user.kind.clone();
        let association = comment
            .author_association
            .clone()
            .unwrap_or_else(|| "NONE".to_owned());
        let seed = GitHubReviewAnchorSeedV1 {
            comment_id: comment_id.clone(),
            author_node_id: user_node_id,
            body_digest: body_digest.clone(),
            safe_url: comment.html_url,
            path: comment.path,
            original_commit_id,
            observed_commit_id,
            original_start_line: comment.original_start_line,
            original_line: comment.original_line,
            current_start_line: comment.start_line,
            current_line: comment.line,
        };
        let anchors = self.anchors.resolve(request, &seed).await?;
        if !anchors.validate()
            || anchors.original.repository_id != request.scope.repository_id
            || anchors.original.commit_id != seed.original_commit_id
            || anchors.initial_remap.current_scope != request.scope
        {
            return None;
        }
        let lifecycle = canonical_lifecycle(provider_lifecycle, &anchors.initial_remap);
        Some(GitHubReviewItemV1 {
            provider: self.identity.provider.clone(),
            repository_id: request.scope.repository_id.clone(),
            pull_request_id: request.pull_request_id.clone(),
            review_id,
            thread_id: None,
            comment_id,
            reply_to_comment_id: comment
                .in_reply_to_id
                .map(|id| GitHubReviewCommentIdV1::new(id.to_string()))
                .transpose()
                .ok()?,
            version_digest,
            author_anchor: anchors.author_anchor,
            author_class: author_class(&user_kind, &association),
            review_state: GitHubReviewStateV1::Unknown,
            body_digest,
            body_anchor: anchors.body_anchor,
            safe_url_anchor: anchors.safe_url_anchor,
            safe_url: (!seed.safe_url.is_empty()).then_some(seed.safe_url),
            lifecycle,
            provider_outcome: outcome,
            remap: anchors.initial_remap,
            observed_at,
        })
    }

    async fn decode_graphql_threads(
        &self,
        request: &GitHubReviewReadRequestV1,
        body: &[u8],
        outcome: GitHubReviewIngressProviderOutcomeV1,
        observed_at: UtcMicros,
    ) -> Option<Vec<GitHubReviewItemV1>> {
        let response = serde_json::from_slice::<GraphQlResponseV1>(body).ok()?;
        if !response.errors.is_empty() {
            return None;
        }
        let pull_request = response.data?.repository?.pull_request?;
        if pull_request.base_ref_oid != self.identity.base_commit_id.as_str()
            || pull_request.head_ref_oid != self.identity.head_commit_id.as_str()
        {
            return None;
        }
        let mut items = Vec::new();
        let mut thread_ids = BTreeSet::new();
        let mut comment_ids = BTreeSet::new();
        for mut thread in pull_request.review_threads.nodes {
            if !thread_ids.insert(thread.id.clone()) || thread.comments.page_info.has_next_page {
                return None;
            }
            let comments = std::mem::take(&mut thread.comments.nodes);
            for comment in comments {
                if !comment_ids.insert(comment.database_id) {
                    return None;
                }
                items.push(
                    self.graphql_comment_item(request, &thread, comment, outcome, observed_at)
                        .await?,
                );
            }
        }
        Some(items)
    }

    async fn graphql_comment_item(
        &self,
        request: &GitHubReviewReadRequestV1,
        thread: &GraphQlReviewThreadV1,
        comment: GraphQlReviewCommentV1,
        outcome: GitHubReviewIngressProviderOutcomeV1,
        observed_at: UtcMicros,
    ) -> Option<GitHubReviewItemV1> {
        let provider_lifecycle = graphql_lifecycle(thread, &comment);
        let comment_id = GitHubReviewCommentIdV1::new(comment.database_id.to_string()).ok()?;
        let review_id = comment
            .pull_request_review
            .as_ref()
            .and_then(|review| review.database_id)
            .map(|id| GitHubReviewIdV1::new(id.to_string()))
            .transpose()
            .ok()?;
        let original_commit_id = CommitId::new(comment.original_commit?.oid).ok()?;
        let observed_commit_id = CommitId::new(
            comment
                .pull_request_review
                .as_ref()?
                .commit
                .as_ref()?
                .oid
                .clone(),
        )
        .ok()?;
        let body_digest = body_digest(comment.body_text.as_deref()?)?;
        let version_digest = review_version_digest(
            &comment_id,
            &comment.updated_at,
            &body_digest,
            &observed_commit_id,
        )?;
        self.identity
            .admits_review_url(&comment.url)
            .then_some(())?;
        let seed = GitHubReviewAnchorSeedV1 {
            comment_id: comment_id.clone(),
            author_node_id: comment.author.as_ref()?.id.clone(),
            body_digest: body_digest.clone(),
            safe_url: comment.url,
            path: thread.path.clone(),
            original_commit_id,
            observed_commit_id,
            original_start_line: thread.original_start_line,
            original_line: thread.original_line,
            current_start_line: thread.start_line,
            current_line: thread.line,
        };
        let anchors = self.anchors.resolve(request, &seed).await?;
        if !anchors.validate()
            || anchors.original.repository_id != request.scope.repository_id
            || anchors.original.commit_id != seed.original_commit_id
            || anchors.initial_remap.current_scope != request.scope
        {
            return None;
        }
        let lifecycle = canonical_lifecycle(provider_lifecycle, &anchors.initial_remap);
        Some(GitHubReviewItemV1 {
            provider: self.identity.provider.clone(),
            repository_id: request.scope.repository_id.clone(),
            pull_request_id: request.pull_request_id.clone(),
            review_id,
            thread_id: Some(GitHubReviewThreadIdV1::new(thread.id.clone()).ok()?),
            comment_id,
            reply_to_comment_id: comment
                .reply_to
                .and_then(|reply| reply.database_id)
                .map(|id| GitHubReviewCommentIdV1::new(id.to_string()))
                .transpose()
                .ok()?,
            version_digest,
            author_anchor: anchors.author_anchor,
            author_class: author_class(
                comment
                    .author
                    .as_ref()
                    .and_then(|author| author.kind.as_deref())
                    .unwrap_or("User"),
                &comment.author_association,
            ),
            review_state: comment
                .pull_request_review
                .as_ref()
                .and_then(|review| review.state.as_deref())
                .and_then(review_state)
                .unwrap_or(GitHubReviewStateV1::Unknown),
            body_digest,
            body_anchor: anchors.body_anchor,
            safe_url_anchor: anchors.safe_url_anchor,
            safe_url: (!seed.safe_url.is_empty()).then_some(seed.safe_url),
            lifecycle,
            provider_outcome: outcome,
            remap: anchors.initial_remap,
            observed_at,
        })
    }
}

fn provider_state(
    status: GitHubReadNetworkStatusV1,
    has_next_page: bool,
) -> (GitHubReviewIngressProviderOutcomeV1, GitHubReviewCoverageV1) {
    match status {
        GitHubReadNetworkStatusV1::Ok if has_next_page => (
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
            GitHubReviewCoverageV1::Unavailable,
        ),
    }
}

fn body_digest(body: &str) -> Option<ManifestDigest> {
    let digest = Sha256::digest(body.as_bytes());
    ManifestDigest::new(format!("sha256:{}", hex::encode(digest))).ok()
}

fn review_version_digest(
    comment_id: &GitHubReviewCommentIdV1,
    updated_at: &str,
    body_digest: &ManifestDigest,
    observed_commit_id: &CommitId,
) -> Option<ManifestDigest> {
    (!updated_at.is_empty() && updated_at.len() <= 64 && !updated_at.chars().any(char::is_control))
        .then_some(())?;
    tracedecay_domain::canonical_sha256(&(
        "tracedecay.pr13.github.review-version.v1",
        comment_id,
        updated_at,
        body_digest,
        observed_commit_id,
    ))
    .ok()
}

fn now_micros() -> Option<UtcMicros> {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_micros();
    Some(UtcMicros(i64::try_from(micros).ok()?))
}

fn review_state(state: &str) -> Option<GitHubReviewStateV1> {
    match state {
        "APPROVED" => Some(GitHubReviewStateV1::Approved),
        "CHANGES_REQUESTED" => Some(GitHubReviewStateV1::ChangesRequested),
        "COMMENTED" => Some(GitHubReviewStateV1::Commented),
        "DISMISSED" => Some(GitHubReviewStateV1::Dismissed),
        "PENDING" => Some(GitHubReviewStateV1::Pending),
        _ => None,
    }
}

fn author_class(kind: &str, association: &str) -> GitHubReviewAuthorClassV1 {
    if kind == "Bot" {
        GitHubReviewAuthorClassV1::Bot
    } else if matches!(association, "OWNER" | "MEMBER" | "COLLABORATOR") {
        GitHubReviewAuthorClassV1::Maintainer
    } else {
        GitHubReviewAuthorClassV1::OtherObservedRole
    }
}

fn rest_lifecycle(comment: &RestReviewCommentV1) -> GitHubReviewLifecycleV1 {
    if comment.line.is_none() {
        GitHubReviewLifecycleV1::Outdated
    } else if comment.created_at != comment.updated_at {
        GitHubReviewLifecycleV1::Edited
    } else {
        GitHubReviewLifecycleV1::Current
    }
}

fn graphql_lifecycle(
    thread: &GraphQlReviewThreadV1,
    comment: &GraphQlReviewCommentV1,
) -> GitHubReviewLifecycleV1 {
    if thread.is_resolved {
        GitHubReviewLifecycleV1::Resolved
    } else if thread.is_outdated {
        GitHubReviewLifecycleV1::Outdated
    } else if comment.created_at != comment.updated_at {
        GitHubReviewLifecycleV1::Edited
    } else {
        GitHubReviewLifecycleV1::Current
    }
}

fn canonical_lifecycle(
    lifecycle: GitHubReviewLifecycleV1,
    remap: &GitHubReviewCurrentBranchRemapV1,
) -> GitHubReviewLifecycleV1 {
    if lifecycle == GitHubReviewLifecycleV1::Current
        && remap.state != GitHubReviewRemapStateV1::ExactCurrent
    {
        GitHubReviewLifecycleV1::Outdated
    } else {
        lifecycle
    }
}

fn valid_repository_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
