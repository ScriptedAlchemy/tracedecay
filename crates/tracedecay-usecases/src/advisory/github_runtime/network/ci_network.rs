//! Read-only GitHub Actions pagination and transport mapping.
//!
//! This module owns the fixed CI GET routes. It shares the parent network
//! authority's credential, timeout, cancellation, and response-decoding
//! boundaries; it cannot form a write request.

use tracedecay_application::feedback::FeedbackPortFuture;
use tracedecay_application::{RequestAdmission, RequestContext, now_micros};
use tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1;

use super::{
    GitHubCiRepositoryTargetV1, GitHubCredentialAuthorizationV1, GitHubHttpReadClientV1,
    GitHubReadOnlyCredentialV1, GitHubReadPermissionV1, HttpResponseV1, MAX_CI_RESPONSE_BYTES_V1,
    MAX_GITHUB_READ_RESPONSE_BYTES_V1, valid_ci_page, valid_full_commit_id,
};

#[derive(Clone)]
pub struct GitHubCiReadOnlyClientV1 {
    pub(super) target: GitHubCiRepositoryTargetV1,
    pub(super) credential: GitHubReadOnlyCredentialV1,
    pub(super) transport: GitHubHttpReadClientV1,
}

impl GitHubCiReadOnlyClientV1 {
    pub fn new(
        target: GitHubCiRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        transport: GitHubHttpReadClientV1,
    ) -> Option<Self> {
        if !target.validate()
            || matches!(
                credential.authorization_for_repository(
                    &target.owner,
                    &target.repository,
                    GitHubReadPermissionV1::Actions,
                ),
                GitHubCredentialAuthorizationV1::Denied
            )
            || matches!(
                credential.authorization_for_repository(
                    &target.owner,
                    &target.repository,
                    GitHubReadPermissionV1::Checks,
                ),
                GitHubCredentialAuthorizationV1::Denied
            )
        {
            return None;
        }
        Some(Self {
            target,
            credential,
            transport,
        })
    }

    async fn get(
        &self,
        context: &RequestContext,
        url: &str,
        permission: GitHubReadPermissionV1,
    ) -> HttpResponseV1 {
        self.transport
            .execute(
                Some(context),
                MAX_GITHUB_READ_RESPONSE_BYTES_V1,
                || {
                    self.credential.authorization_header_for_repository(
                        &self.target.owner,
                        &self.target.repository,
                        permission,
                    )
                },
                |client| {
                    client
                        .get(url)
                        .header("Accept", "application/vnd.github+json")
                        .header("X-GitHub-Api-Version", "2022-11-28")
                        .header("User-Agent", "tracedecay-github-read")
                },
            )
            .await
    }

    pub(crate) fn read_workflow_run<'a>(
        &'a self,
        context: &'a RequestContext,
        run_id: u64,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if run_id == 0 {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/runs/{run_id}",
                self.transport.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository
            ),
        )
    }

    pub(crate) fn read_workflow_runs_for_head<'a>(
        &'a self,
        context: &'a RequestContext,
        head_sha: &str,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if !valid_full_commit_id(head_sha) || !valid_ci_page(page) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        let encoded_head =
            url::form_urlencoded::byte_serialize(head_sha.as_bytes()).collect::<String>();
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/runs?head_sha={encoded_head}&per_page=100&page={}",
                self.transport.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository,
                page
            ),
        )
    }

    pub(crate) fn read_check_run<'a>(
        &'a self,
        context: &'a RequestContext,
        check_run_id: u64,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if check_run_id == 0 {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Checks,
            format!(
                "{}/repos/{}/{}/check-runs/{check_run_id}",
                self.transport.config.rest_base_uri.trim_end_matches('/'),
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
        if job_id == 0 {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/jobs/{job_id}",
                self.transport.config.rest_base_uri.trim_end_matches('/'),
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
        if run_id == 0 || !valid_ci_page(page) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/runs/{run_id}/jobs?per_page=100&page={}",
                self.transport.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository,
                page
            ),
        )
    }

    pub(crate) fn read_check_runs<'a>(
        &'a self,
        context: &'a RequestContext,
        check_suite_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if check_suite_id == 0 || !valid_ci_page(page) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Checks,
            format!(
                "{}/repos/{}/{}/check-suites/{check_suite_id}/check-runs?status=completed&filter=latest&per_page=100&page={}",
                self.transport.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository,
                page
            ),
        )
    }

    pub(crate) fn read_check_annotations<'a>(
        &'a self,
        context: &'a RequestContext,
        check_run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if check_run_id == 0 || !valid_ci_page(page) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Checks,
            format!(
                "{}/repos/{}/{}/check-runs/{check_run_id}/annotations?per_page=100&page={}",
                self.transport.config.rest_base_uri.trim_end_matches('/'),
                self.target.owner,
                self.target.repository,
                page
            ),
        )
    }

    fn read_ci_get<'a>(
        &'a self,
        context: &'a RequestContext,
        permission: GitHubReadPermissionV1,
        url: String,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        if !matches!(
            context.admission_at(now_micros()),
            RequestAdmission::Admitted
        ) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        if matches!(
            self.credential.authorization_for_repository(
                &self.target.owner,
                &self.target.repository,
                permission,
            ),
            GitHubCredentialAuthorizationV1::Denied
        ) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Denied });
        }
        Box::pin(async move {
            match self.get(context, &url, permission).await {
                HttpResponseV1::Ok { body, .. } if body.len() <= MAX_CI_RESPONSE_BYTES_V1 => {
                    GitHubCiTransportOutcomeV1::Response(body)
                }
                HttpResponseV1::RateLimited {
                    checkpoint: Some(limit),
                    ..
                } => GitHubCiTransportOutcomeV1::RateLimited(limit),
                HttpResponseV1::Denied => GitHubCiTransportOutcomeV1::Denied,
                _ => GitHubCiTransportOutcomeV1::Unavailable,
            }
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
