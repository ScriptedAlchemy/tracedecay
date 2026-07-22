#![allow(dead_code)] // in-flight feature APIs not yet wired; see clippy sweep
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::de::DeserializeOwned;
use serde_json::json;
use tracedecay_application::feedback::FeedbackPortFuture;
#[cfg(test)]
use tracedecay_application::feedback::GitHubReviewReadRequestV1;
use tracedecay_application::{RequestAdmission, RequestContext};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::feedback::{
    GitHubReviewCursorV1, GitHubReviewEtagV1, GitHubReviewRateLimitCheckpointV1,
    GitHubReviewReadOperationV1,
};
use url::Url;

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
              author { __typename id }
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
          author { __typename id }
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
const MAX_CI_RESPONSE_BYTES_V1: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitHubReadPermissionV1 {
    Metadata,
    PullRequests,
    Contents,
    Actions,
    Checks,
}

impl GitHubReadPermissionV1 {
    pub fn parse(scope: &str) -> Option<Self> {
        match scope {
            "metadata:read" => Some(Self::Metadata),
            "pull_requests:read" => Some(Self::PullRequests),
            "contents:read" => Some(Self::Contents),
            "actions:read" => Some(Self::Actions),
            "checks:read" => Some(Self::Checks),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct GitHubReadOnlyCredentialV1 {
    token: Option<String>,
    permissions: BTreeSet<GitHubReadPermissionV1>,
}

impl GitHubReadOnlyCredentialV1 {
    pub fn anonymous() -> Self {
        Self {
            token: None,
            permissions: BTreeSet::new(),
        }
    }

    pub fn from_declared_scopes(
        token: String,
        declared_scopes: impl IntoIterator<Item = String>,
    ) -> Option<Self> {
        if token.trim().is_empty() {
            return None;
        }
        let permissions = declared_scopes
            .into_iter()
            .map(|scope| GitHubReadPermissionV1::parse(scope.trim()))
            .collect::<Option<BTreeSet<_>>>()?;
        (!permissions.is_empty()).then_some(Self {
            token: Some(token),
            permissions,
        })
    }

    fn permits(&self, permission: GitHubReadPermissionV1) -> bool {
        self.token.is_none() || self.permissions.contains(&permission)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubRepositoryTargetV1 {
    pub owner: String,
    pub repository: String,
    pub pull_request_number: u64,
    pub pull_request_id: tracedecay_domain::feedback::GitHubPullRequestIdV1,
}

impl GitHubRepositoryTargetV1 {
    pub fn validate(&self) -> bool {
        valid_path_segment(&self.owner)
            && valid_path_segment(&self.repository)
            && self.pull_request_number > 0
            && i32::try_from(self.pull_request_number).is_ok()
            && self.pull_request_id.validate().is_ok()
    }
}

#[derive(Clone, Debug)]
pub struct GitHubHttpReadConfigV1 {
    pub rest_base_uri: String,
    pub graphql_uri: String,
    pub request_timeout: Duration,
    pub connect_timeout: Duration,
    pub socket_timeout: Duration,
}

impl Default for GitHubHttpReadConfigV1 {
    fn default() -> Self {
        Self {
            rest_base_uri: "https://api.github.com".to_owned(),
            graphql_uri: "https://api.github.com/graphql".to_owned(),
            request_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            socket_timeout: Duration::from_secs(20),
        }
    }
}

impl GitHubHttpReadConfigV1 {
    fn validate(&self) -> bool {
        let (Ok(rest), Ok(graphql)) = (
            Url::parse(&self.rest_base_uri),
            Url::parse(&self.graphql_uri),
        ) else {
            return false;
        };
        rest.scheme() == "https"
            && graphql.scheme() == "https"
            && rest.host_str() == graphql.host_str()
            && rest.port_or_known_default() == graphql.port_or_known_default()
            && !self.request_timeout.is_zero()
            && !self.connect_timeout.is_zero()
            && !self.socket_timeout.is_zero()
    }
}

#[derive(Clone)]
pub struct GitHubReadOnlyClientV1 {
    agent: ureq::Agent,
    target: GitHubRepositoryTargetV1,
    credential: GitHubReadOnlyCredentialV1,
    config: GitHubHttpReadConfigV1,
}

impl GitHubReadOnlyClientV1 {
    pub fn new(
        target: GitHubRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<Self> {
        if !credential.permits(GitHubReadPermissionV1::PullRequests) {
            return None;
        }
        Self::build(target, credential, config)
    }

    pub fn new_for_ci(
        target: GitHubRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<Self> {
        if !credential.permits(GitHubReadPermissionV1::Actions)
            || !credential.permits(GitHubReadPermissionV1::Checks)
        {
            return None;
        }
        Self::build(target, credential, config)
    }

    fn build(
        target: GitHubRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<Self> {
        if !target.validate() || !config.validate() {
            return None;
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(config.request_timeout))
            .timeout_connect(Some(config.connect_timeout))
            .timeout_recv_response(Some(config.socket_timeout))
            .timeout_recv_body(Some(config.socket_timeout))
            .https_only(true)
            .max_redirects(0)
            .http_status_as_error(false)
            .build()
            .into();
        Some(Self {
            agent,
            target,
            credential,
            config,
        })
    }

    fn execute_rest(&self, request: &GitHubRestReadRequestV1) -> GitHubReadNetworkOutcomeV1 {
        if request.pull_request_id != self.target.pull_request_id {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        let Some(page) = page_from_cursor(request.resume.cursor.as_ref()) else {
            return GitHubReadNetworkOutcomeV1::Unavailable;
        };
        let suffix = match request.descriptor.operation {
            GitHubReviewReadOperationV1::RestGetPullRequest => String::new(),
            GitHubReviewReadOperationV1::RestListPullRequestReviews => {
                format!("/reviews?per_page=100&page={page}")
            }
            GitHubReviewReadOperationV1::RestListPullRequestReviewComments => {
                format!("/comments?per_page=100&page={page}")
            }
            GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads => {
                return GitHubReadNetworkOutcomeV1::Unavailable;
            }
        };
        let url = format!(
            "{}/repos/{}/{}/pulls/{}{}",
            self.config.rest_base_uri.trim_end_matches('/'),
            self.target.owner,
            self.target.repository,
            self.target.pull_request_number,
            suffix
        );
        let response = self.get(&url, request.resume.etag.as_ref());
        Self::decode_rest_response(response, request.descriptor.operation)
    }

    fn execute_graphql(&self, request: &GitHubGraphQlReadRequestV1) -> GitHubReadNetworkOutcomeV1 {
        if request.pull_request_id != self.target.pull_request_id
            || request
                .resume
                .cursor
                .as_ref()
                .is_some_and(|cursor| cursor.as_str().starts_with("rest-page:"))
        {
            return GitHubReadNetworkOutcomeV1::Denied;
        }
        let variables = json!({
            "owner": self.target.owner,
            "repository": self.target.repository,
            "number": self.target.pull_request_number,
            "threadAfter": request.resume.cursor.as_ref().map(GitHubReviewCursorV1::as_str),
            "commentThreadId": "unused",
            "commentAfter": null,
            "loadThreads": true,
            "loadComments": false,
        });
        let mut envelope = match self.graphql(&variables) {
            Ok(envelope) => envelope,
            Err(failure) => return network_failure(failure),
        };
        if !envelope.errors.is_empty() {
            return GitHubReadNetworkOutcomeV1::Unavailable;
        }
        if let Err(failure) = self.complete_nested_comment_pages(&mut envelope) {
            return network_failure(failure);
        }
        let next_cursor = envelope
            .data
            .as_ref()
            .and_then(|data| data.repository.as_ref())
            .and_then(|repository| repository.pull_request.as_ref())
            .and_then(|pull_request| {
                pull_request
                    .review_threads
                    .page_info
                    .has_next_page
                    .then_some(pull_request.review_threads.page_info.end_cursor.as_deref())
                    .flatten()
            })
            .and_then(|cursor| GitHubReviewCursorV1::new(cursor).ok());
        let Ok(body) = serde_json::to_vec(&envelope) else {
            return GitHubReadNetworkOutcomeV1::Unavailable;
        };
        if body.len() > MAX_GITHUB_READ_RESPONSE_BYTES_V1 {
            return GitHubReadNetworkOutcomeV1::Unavailable;
        }
        GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
            metadata: GitHubReadNetworkMetadataV1 {
                status: GitHubReadNetworkStatusV1::Ok,
                etag: None,
                next_cursor,
                rate_limit: None,
            },
            body,
        })
    }

    fn complete_nested_comment_pages(
        &self,
        envelope: &mut GraphQlResponseV1,
    ) -> Result<(), HttpResponseV1> {
        let Some(threads) = envelope
            .data
            .as_mut()
            .and_then(|data| data.repository.as_mut())
            .and_then(|repository| repository.pull_request.as_mut())
            .map(|pull_request| &mut pull_request.review_threads.nodes)
        else {
            return Err(HttpResponseV1::Unavailable);
        };
        let mut total = threads
            .iter()
            .map(|thread| thread.comments.nodes.len())
            .sum::<usize>();
        for thread in threads {
            let mut pages = 0_usize;
            while thread.comments.page_info.has_next_page {
                pages += 1;
                if pages > MAX_NESTED_COMMENT_PAGES_V1 || total >= MAX_REVIEW_ITEMS_V1 {
                    return Err(HttpResponseV1::Unavailable);
                }
                let Some(comment_after) = thread.comments.page_info.end_cursor.clone() else {
                    return Err(HttpResponseV1::Unavailable);
                };
                let variables = json!({
                    "owner": self.target.owner,
                    "repository": self.target.repository,
                    "number": self.target.pull_request_number,
                    "threadAfter": null,
                    "commentThreadId": thread.id.clone(),
                    "commentAfter": comment_after,
                    "loadThreads": false,
                    "loadComments": true,
                });
                let page = self.graphql(&variables)?;
                if !page.errors.is_empty() {
                    return Err(HttpResponseV1::Unavailable);
                }
                let Some(GraphQlCommentPageNodeV1 { id, comments }) =
                    page.data.and_then(|data| data.node)
                else {
                    return Err(HttpResponseV1::Unavailable);
                };
                if id != thread.id || comments.nodes.is_empty() {
                    return Err(HttpResponseV1::Unavailable);
                }
                total = total.saturating_add(comments.nodes.len());
                if total > MAX_REVIEW_ITEMS_V1 {
                    return Err(HttpResponseV1::Unavailable);
                }
                thread.comments.nodes.extend(comments.nodes);
                thread.comments.page_info = comments.page_info;
            }
        }
        Ok(())
    }

    fn graphql(&self, variables: &serde_json::Value) -> Result<GraphQlResponseV1, HttpResponseV1> {
        let payload = json!({
            "query": GITHUB_REVIEW_THREADS_QUERY_V1,
            "variables": variables,
        });
        let response = self.post_static_graphql(&payload);
        match response {
            HttpResponseV1::Ok { body, .. } => {
                serde_json::from_slice(&body).map_err(|_| HttpResponseV1::Unavailable)
            }
            failure => Err(failure),
        }
    }

    fn decode_rest_response(
        response: HttpResponseV1,
        operation: GitHubReviewReadOperationV1,
    ) -> GitHubReadNetworkOutcomeV1 {
        match response {
            HttpResponseV1::Ok {
                body,
                etag,
                next_page,
                rate_limit,
            } => {
                let valid = match operation {
                    GitHubReviewReadOperationV1::RestGetPullRequest => {
                        parse_bounded::<RestPullRequestV1>(&body).is_some()
                    }
                    GitHubReviewReadOperationV1::RestListPullRequestReviews => {
                        parse_bounded::<Vec<RestReviewV1>>(&body)
                            .is_some_and(|items| items.len() <= 100)
                    }
                    GitHubReviewReadOperationV1::RestListPullRequestReviewComments => {
                        parse_bounded::<Vec<RestReviewCommentV1>>(&body)
                            .is_some_and(|items| items.len() <= 100)
                    }
                    GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads => false,
                };
                if !valid {
                    return GitHubReadNetworkOutcomeV1::Unavailable;
                }
                GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::Ok,
                        etag,
                        next_cursor: next_page.and_then(|page| {
                            GitHubReviewCursorV1::new(format!("rest-page:{page}")).ok()
                        }),
                        rate_limit,
                    },
                    body,
                })
            }
            HttpResponseV1::NotModified { etag, rate_limit } => {
                GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::NotModified,
                        etag,
                        next_cursor: None,
                        rate_limit,
                    },
                    body: Vec::new(),
                })
            }
            HttpResponseV1::RateLimited(checkpoint) => {
                GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                    metadata: GitHubReadNetworkMetadataV1 {
                        status: GitHubReadNetworkStatusV1::RateLimited,
                        etag: None,
                        next_cursor: None,
                        rate_limit: Some(checkpoint),
                    },
                    body: Vec::new(),
                })
            }
            HttpResponseV1::Denied => GitHubReadNetworkOutcomeV1::Denied,
            HttpResponseV1::Unavailable => GitHubReadNetworkOutcomeV1::Unavailable,
        }
    }

    fn get(&self, url: &str, etag: Option<&GitHubReviewEtagV1>) -> HttpResponseV1 {
        let mut request = self
            .agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tracedecay-github-read");
        if let Some(token) = self.credential.token.as_ref() {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        if let Some(etag) = etag {
            request = request.header("If-None-Match", etag.as_str());
        }
        decode_ureq_response(request.call(), MAX_GITHUB_READ_RESPONSE_BYTES_V1)
    }

    fn post_static_graphql(&self, payload: &serde_json::Value) -> HttpResponseV1 {
        let mut request = self
            .agent
            .post(&self.config.graphql_uri)
            .header("Accept", "application/json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tracedecay-github-read");
        if let Some(token) = self.credential.token.as_ref() {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        decode_ureq_response(
            request.send_json(payload),
            MAX_GITHUB_READ_RESPONSE_BYTES_V1,
        )
    }

    pub(crate) fn read_workflow_run<'a>(
        &'a self,
        context: &'a RequestContext,
        run_id: u64,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/runs/{run_id}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository
            ),
        )
    }

    pub(crate) fn read_check_run<'a>(
        &'a self,
        context: &'a RequestContext,
        check_run_id: u64,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Checks,
            format!(
                "{}/repos/{}/{}/check-runs/{check_run_id}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository
            ),
        )
    }

    pub(crate) fn read_workflow_job<'a>(
        &'a self,
        context: &'a RequestContext,
        job_id: u64,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/jobs/{job_id}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository
            ),
        )
    }

    pub(crate) fn read_workflow_jobs<'a>(
        &'a self,
        context: &'a RequestContext,
        run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/runs/{run_id}/jobs?per_page=100&page={}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository,
                page.max(1)
            ),
        )
    }

    pub(crate) fn read_check_annotations<'a>(
        &'a self,
        context: &'a RequestContext,
        check_run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Checks,
            format!(
                "{}/repos/{}/{}/check-runs/{check_run_id}/annotations?per_page=100&page={}",
                self.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository,
                page.max(1)
            ),
        )
    }

    fn read_ci_get<'a>(
        &'a self,
        context: &'a RequestContext,
        permission: GitHubReadPermissionV1,
        url: String,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if !self.credential.permits(permission) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Denied });
        }
        let client = self.clone();
        Box::pin(async move {
            let task = tokio::task::spawn_blocking(move || client.get(&url, None));
            match wait_for_read(context, task).await {
                Some(HttpResponseV1::Ok { body, .. }) if body.len() <= MAX_CI_RESPONSE_BYTES_V1 => {
                    GitHubCiTransportOutcomeV1::Response(body)
                }
                Some(HttpResponseV1::RateLimited(limit)) => {
                    GitHubCiTransportOutcomeV1::RateLimited(limit)
                }
                Some(HttpResponseV1::Denied) => GitHubCiTransportOutcomeV1::Denied,
                _ => GitHubCiTransportOutcomeV1::Unavailable,
            }
        })
    }
}

impl GitHubReadOnlyNetworkAuthorityV1 for GitHubReadOnlyClientV1 {
    fn get<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubRestReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1> {
        let client = self.clone();
        let request = request.clone();
        Box::pin(async move {
            let task = tokio::task::spawn_blocking(move || client.execute_rest(&request));
            wait_for_read(context, task)
                .await
                .unwrap_or(GitHubReadNetworkOutcomeV1::Unavailable)
        })
    }

    fn query<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubGraphQlReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReadNetworkOutcomeV1> {
        let client = self.clone();
        let request = request.clone();
        Box::pin(async move {
            let task = tokio::task::spawn_blocking(move || client.execute_graphql(&request));
            wait_for_read(context, task)
                .await
                .unwrap_or(GitHubReadNetworkOutcomeV1::Unavailable)
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubCiTransportOutcomeV1 {
    Response(Vec<u8>),
    RateLimited(GitHubReviewRateLimitCheckpointV1),
    Denied,
    Unavailable,
}

enum HttpResponseV1 {
    Ok {
        body: Vec<u8>,
        etag: Option<GitHubReviewEtagV1>,
        next_page: Option<u32>,
        rate_limit: Option<GitHubReviewRateLimitCheckpointV1>,
    },
    NotModified {
        etag: Option<GitHubReviewEtagV1>,
        rate_limit: Option<GitHubReviewRateLimitCheckpointV1>,
    },
    RateLimited(GitHubReviewRateLimitCheckpointV1),
    Denied,
    Unavailable,
}

fn network_failure(failure: HttpResponseV1) -> GitHubReadNetworkOutcomeV1 {
    match failure {
        HttpResponseV1::RateLimited(checkpoint) => {
            GitHubReadNetworkOutcomeV1::Response(GitHubReadNetworkResponseV1 {
                metadata: GitHubReadNetworkMetadataV1 {
                    status: GitHubReadNetworkStatusV1::RateLimited,
                    etag: None,
                    next_cursor: None,
                    rate_limit: Some(checkpoint),
                },
                body: Vec::new(),
            })
        }
        HttpResponseV1::Denied => GitHubReadNetworkOutcomeV1::Denied,
        _ => GitHubReadNetworkOutcomeV1::Unavailable,
    }
}

fn decode_ureq_response(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    maximum: usize,
) -> HttpResponseV1 {
    let Ok(mut response) = response else {
        return HttpResponseV1::Unavailable;
    };
    let rate_limit = rate_limit_checkpoint(response.headers());
    match response.status().as_u16() {
        200 => {
            let etag = header(response.headers(), "etag")
                .and_then(|value| GitHubReviewEtagV1::new(value).ok());
            let next_page = next_page(response.headers());
            let Ok(body) = response
                .body_mut()
                .with_config()
                .limit(maximum as u64)
                .read_to_vec()
            else {
                return HttpResponseV1::Unavailable;
            };
            HttpResponseV1::Ok {
                body,
                etag,
                next_page,
                rate_limit,
            }
        }
        304 => HttpResponseV1::NotModified {
            etag: header(response.headers(), "etag")
                .and_then(|value| GitHubReviewEtagV1::new(value).ok()),
            rate_limit,
        },
        401 => HttpResponseV1::Denied,
        403 | 429 => rate_limit
            .filter(|limit| limit.remaining == 0)
            .map_or(HttpResponseV1::Denied, HttpResponseV1::RateLimited),
        _ => HttpResponseV1::Unavailable,
    }
}

async fn wait_for_read<T: Send + 'static>(
    context: &RequestContext,
    task: tokio::task::JoinHandle<T>,
) -> Option<T> {
    tokio::select! {
        result = task => result.ok(),
        () = wait_for_interruption(context) => None,
    }
}

async fn wait_for_interruption(context: &RequestContext) {
    loop {
        if !matches!(
            context.admission_at(now_micros()),
            RequestAdmission::Admitted
        ) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn page_from_cursor(cursor: Option<&GitHubReviewCursorV1>) -> Option<u32> {
    match cursor {
        Some(cursor) => cursor
            .as_str()
            .strip_prefix("rest-page:")
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|page| *page > 0),
        None => Some(1),
    }
}

fn next_page(headers: &ureq::http::HeaderMap) -> Option<u32> {
    let link = header(headers, "link")?;
    let next = link
        .split(',')
        .find(|entry| entry.contains("rel=\"next\""))?;
    let url = next.split_once('<')?.1.split_once('>')?.0;
    Url::parse(url)
        .ok()?
        .query_pairs()
        .find_map(|(key, value)| (key == "page").then(|| value.parse::<u32>().ok()).flatten())
}

fn rate_limit_checkpoint(
    headers: &ureq::http::HeaderMap,
) -> Option<GitHubReviewRateLimitCheckpointV1> {
    let checkpoint = GitHubReviewRateLimitCheckpointV1 {
        limit: header(headers, "x-ratelimit-limit")?.parse().ok()?,
        remaining: header(headers, "x-ratelimit-remaining")?.parse().ok()?,
        reset_at: UtcMicros(
            header(headers, "x-ratelimit-reset")?
                .parse::<i64>()
                .ok()?
                .checked_mul(1_000_000)?,
        ),
    };
    checkpoint.validate().is_ok().then_some(checkpoint)
}

fn parse_bounded<T: DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    (bytes.len() <= MAX_GITHUB_READ_RESPONSE_BYTES_V1)
        .then(|| serde_json::from_slice(bytes).ok())
        .flatten()
}

fn header(headers: &ureq::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

fn now_micros() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros())
        .unwrap_or_default();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

fn valid_path_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && value != "."
        && value != ".."
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::feedback::{
        FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewCoverageV1,
        GitHubReviewIngressProviderOutcomeV1, GitHubReviewIngressResultV1,
        GitHubReviewReadCheckpointV1,
    };
    use tracedecay_domain::{
        ActorId, CommitId, ManifestDigest, ProjectId, ProviderId, RefId, RepositoryId, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::super::store::ProjectGitHubReviewStoreV1;
    use super::super::{
        GitHubReadResumeV1, GitHubReviewAtomicRefreshStoreV1, GitHubReviewReadResponseV1,
        GitHubReviewRefreshStateV1, GitHubReviewRefreshStoreCommitOutcomeV1,
    };
    use super::*;
    use crate::db::{Database, DatabaseAuthority};

    const SHA: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const THREAD_CAPTURE: &str =
        include_str!("../fixtures/pr13_branch_pr/review_thread.graphql.json");

    fn scope(suffix: &str) -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: ProjectId::new(format!("project.github.{suffix}")).unwrap(),
            repository_id: RepositoryId::new(format!("repository.github.{suffix}")).unwrap(),
            worktree_id: WorktreeId::new(format!("worktree.github.{suffix}")).unwrap(),
            branch_ref: format!("refs/heads/github-{suffix}"),
            head_commit_id: CommitId::new(format!("commit.github.{suffix}.head")).unwrap(),
        }
    }

    fn context(scope: &FeedbackScopeV1) -> RequestContext {
        let resolved = ResolvedScope::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            Some(RefId::new(scope.branch_ref.clone()).unwrap()),
        )
        .unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.github.owner-bound").unwrap(),
            1,
            ManifestDigest::new(SHA).unwrap(),
            ActorId::new("actor.github.issuer").unwrap(),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            resolved.clone(),
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
        RequestContext::new(
            ActorId::new("actor.github.owner-bound").unwrap(),
            resolved,
            grant,
            RequestId::new("request.github.owner-bound").unwrap(),
            Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
            CancellationContext::active("cancel.github.owner-bound").unwrap(),
        )
        .unwrap()
    }

    fn request(scope: FeedbackScopeV1) -> GitHubReviewReadRequestV1 {
        GitHubReviewReadRequestV1 {
            operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
            scope,
            pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
        }
    }

    fn complete_response(request: &GitHubReviewReadRequestV1) -> GitHubReviewReadResponseV1 {
        GitHubReviewReadResponseV1 {
            ingress: GitHubReviewIngressResultV1 {
                provider: ProviderId::new("provider.github").unwrap(),
                scope: request.scope.clone(),
                pull_request_id: request.pull_request_id.clone(),
                provider_base_commit_id: CommitId::new("commit.github.base").unwrap(),
                provider_head_commit_id: request.scope.head_commit_id.clone(),
                merge_base_commit_id: CommitId::new("commit.github.merge-base").unwrap(),
                operation: request.operation,
                outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
                coverage: GitHubReviewCoverageV1::Complete,
                items: Vec::new(),
                fetched_at: UtcMicros(10),
            },
            checkpoint: GitHubReviewReadCheckpointV1 {
                etag: None,
                next_cursor: None,
                rate_limit: None,
            },
        }
    }

    fn read_http_request(stream: &mut TcpStream) -> serde_json::Value {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "fixture client closed before request headers");
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap();
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "fixture client closed before request body");
            bytes.extend_from_slice(&buffer[..read]);
        }
        serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
    }

    fn write_http_json(stream: &mut TcpStream, value: &serde_json::Value) {
        let body = serde_json::to_vec(value).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
    }

    #[tokio::test]
    async fn github_nested_pagination_and_cas_are_owner_bound() {
        let mut first_page: serde_json::Value = serde_json::from_str(THREAD_CAPTURE).unwrap();
        first_page = first_page["response"].take();
        let thread =
            &mut first_page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"][0];
        thread["comments"]["pageInfo"] = json!({
            "hasNextPage": true,
            "endCursor": "cursor.comments.1"
        });
        let thread_id = thread["id"].as_str().unwrap().to_owned();
        let mut next_comment = thread["comments"]["nodes"][0].clone();
        next_comment["databaseId"] = json!(3_556_767_424_u64);
        next_comment["url"] =
            json!("https://github.com/ScriptedAlchemy/tracedecay/pull/421#discussion_r3556767424");
        let second_page = json!({
            "data": {
                "node": {
                    "id": thread_id.clone(),
                    "comments": {
                        "nodes": [next_comment],
                        "pageInfo": {
                            "hasNextPage": false,
                            "endCursor": null
                        }
                    }
                }
            }
        });

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let server = std::thread::spawn(move || {
            for response in [first_page, second_page] {
                let (mut stream, _) = listener.accept().unwrap();
                captured
                    .lock()
                    .unwrap()
                    .push(read_http_request(&mut stream));
                write_http_json(&mut stream, &response);
            }
        });
        let config = GitHubHttpReadConfigV1 {
            rest_base_uri: format!("http://{address}"),
            graphql_uri: format!("http://{address}/graphql"),
            ..GitHubHttpReadConfigV1::default()
        };
        let client = GitHubReadOnlyClientV1 {
            agent: ureq::Agent::config_builder()
                .https_only(false)
                .http_status_as_error(false)
                .build()
                .into(),
            target: GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: "tracedecay".to_owned(),
                pull_request_number: 421,
                pull_request_id: GitHubPullRequestIdV1::new("4026204542").unwrap(),
            },
            credential: GitHubReadOnlyCredentialV1::anonymous(),
            config,
        };
        let owner_scope = scope("owner");
        let read_request = request(owner_scope.clone());
        let outcome = client.execute_graphql(&GitHubGraphQlReadRequestV1 {
            scope: owner_scope.clone(),
            pull_request_id: read_request.pull_request_id.clone(),
            resume: GitHubReadResumeV1::empty(),
        });
        server.join().unwrap();
        let GitHubReadNetworkOutcomeV1::Response(response) = outcome else {
            panic!("production GraphQL client must complete nested pagination");
        };
        let envelope: GraphQlResponseV1 = serde_json::from_slice(&response.body).unwrap();
        let comments = &envelope
            .data
            .unwrap()
            .repository
            .unwrap()
            .pull_request
            .unwrap()
            .review_threads
            .nodes[0]
            .comments;
        assert_eq!(comments.nodes.len(), 2);
        assert!(!comments.page_info.has_next_page);
        {
            let requests = requests.lock().unwrap();
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0]["variables"]["loadThreads"], true);
            assert_eq!(requests[1]["variables"]["loadComments"], true);
            assert_eq!(requests[1]["variables"]["commentThreadId"], thread_id);
            assert_eq!(
                requests[1]["variables"]["commentAfter"],
                "cursor.comments.1"
            );
        }

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("github-owner-bound.db");
        let authority = DatabaseAuthority::acquire_test(&path, "github owner-bound CAS").unwrap();
        let (database, _) = Database::initialize(&path, &authority).await.unwrap();
        let store =
            ProjectGitHubReviewStoreV1::new(database, owner_scope.clone()).expect("owner store");
        let context = context(&owner_scope);
        let state = GitHubReviewRefreshStateV1::transition(
            &read_request,
            None,
            complete_response(&read_request),
        )
        .unwrap();
        assert_eq!(
            store
                .compare_and_record(&context, &read_request, None, &state)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Recorded
        );
        assert_eq!(
            store
                .compare_and_record(&context, &read_request, None, &state)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Duplicate
        );

        let foreign_request = request(scope("foreign"));
        assert_eq!(
            store
                .compare_and_record(&context, &foreign_request, None, &state)
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Unavailable
        );
        let mut latest = complete_response(&read_request);
        latest.ingress.fetched_at = UtcMicros(11);
        let advanced =
            GitHubReviewRefreshStateV1::transition(&read_request, Some(&state), latest).unwrap();
        assert_eq!(
            store
                .compare_and_record(
                    &context,
                    &read_request,
                    Some(&ManifestDigest::new(SHA).unwrap()),
                    &advanced,
                )
                .await,
            GitHubReviewRefreshStoreCommitOutcomeV1::Conflict
        );
    }
}
