//! Read-only GitHub Actions pagination and transport mapping.
//!
//! This module owns the fixed CI GET routes. It shares the parent network
//! authority's credential, timeout, cancellation, and response-decoding
//! boundaries; it cannot form a write request.

use tracedecay_application::feedback::FeedbackPortFuture;
use tracedecay_application::{RequestAdmission, RequestContext, now_micros};
use tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1;

use super::{
    GitHubCiRepositoryTargetV1, GitHubCredentialAuthorizationV1, GitHubHttpReadConfigV1,
    GitHubReadOnlyCredentialV1, GitHubReadPermissionV1, HttpResponseV1, MAX_CI_RESPONSE_BYTES_V1,
    MAX_GITHUB_READ_RESPONSE_BYTES_V1, decode_ureq_response, request_context_admitted,
    valid_ci_page, valid_full_commit_id, wait_for_read,
};

#[derive(Clone)]
pub struct GitHubCiReadOnlyClientV1 {
    pub(super) agent: ureq::Agent,
    pub(super) target: GitHubCiRepositoryTargetV1,
    pub(super) credential: GitHubReadOnlyCredentialV1,
    pub(super) config: GitHubHttpReadConfigV1,
}

impl GitHubCiReadOnlyClientV1 {
    pub(super) fn new(
        target: GitHubCiRepositoryTargetV1,
        credential: GitHubReadOnlyCredentialV1,
        config: GitHubHttpReadConfigV1,
    ) -> Option<Self> {
        if !target.validate()
            || !config.validate()
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

    fn get(&self, url: &str, permission: GitHubReadPermissionV1) -> HttpResponseV1 {
        let authorization = self.credential.authorization_for_repository(
            &self.target.owner,
            &self.target.repository,
            permission,
        );
        if matches!(&authorization, GitHubCredentialAuthorizationV1::Denied) {
            return HttpResponseV1::Denied;
        }
        let mut request = self
            .agent
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "tracedecay-github-read");
        if let GitHubCredentialAuthorizationV1::Private(authorization) = &authorization {
            request = request.header("Authorization", authorization.as_str());
        }
        decode_ureq_response(request.call(), MAX_GITHUB_READ_RESPONSE_BYTES_V1)
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
                self.config.rest_base_uri.trim_end_matches('/'),
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
                self.config.rest_base_uri.trim_end_matches('/'),
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
        if job_id == 0 {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
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
        if run_id == 0 || !valid_ci_page(page) {
            return Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable });
        }
        self.read_ci_get(
            context,
            GitHubReadPermissionV1::Actions,
            format!(
                "{}/repos/{}/{}/actions/runs/{run_id}/jobs?per_page=100&page={}",
                self.config.rest_base_uri.trim_end_matches('/'),
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
                self.config.rest_base_uri.trim_end_matches('/'),
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
                self.config.rest_base_uri.trim_end_matches('/'),
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
        let client = self.clone();
        let context_for_read = context.clone();
        Box::pin(async move {
            let task = tokio::task::spawn_blocking(move || {
                if request_context_admitted(&context_for_read)
                    && !matches!(
                        client.credential.authorization_for_repository(
                            &client.target.owner,
                            &client.target.repository,
                            permission,
                        ),
                        GitHubCredentialAuthorizationV1::Denied
                    )
                {
                    let response = client.get(&url, permission);
                    if request_context_admitted(&context_for_read)
                        && !matches!(
                            client.credential.authorization_for_repository(
                                &client.target.owner,
                                &client.target.repository,
                                permission,
                            ),
                            GitHubCredentialAuthorizationV1::Denied
                        )
                    {
                        response
                    } else {
                        HttpResponseV1::Denied
                    }
                } else {
                    HttpResponseV1::Denied
                }
            });
            match wait_for_read(context, task).await {
                Some(HttpResponseV1::Ok { body, .. }) if body.len() <= MAX_CI_RESPONSE_BYTES_V1 => {
                    GitHubCiTransportOutcomeV1::Response(body)
                }
                Some(HttpResponseV1::RateLimited {
                    checkpoint: Some(limit),
                    ..
                }) => GitHubCiTransportOutcomeV1::RateLimited(limit),
                Some(HttpResponseV1::Denied) => GitHubCiTransportOutcomeV1::Denied,
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
