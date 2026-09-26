use tracedecay_contracts::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, FeedbackPortFuture,
    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GitHubReviewReadRequestV1, feedback_surface_operation,
};
use tracedecay_contracts::{AuthorizationRequest, ResolvedScope, now_micros};
use tracedecay_domain::configuration::{
    AuthorityRef, ScopeSourceBinding, SourceBindingId, SourceKindV1,
};
use tracedecay_domain::feedback::GitHubPullRequestIdV1;
use tracedecay_domain::{LocatorDigest, ProjectId, canonical_sha256};

use super::{GitHubProviderLifecycleV1, GitHubRepositoryTargetV1, GitHubSourceAccessAuthorityV1};
use crate::advisory::ci_runtime::{CiSourceAccessAuthorityV1, CiSourceAccessOutcomeV1};
use crate::source_authorization::{
    ProjectSourceAccessOutcome, project_source_access_snapshot_for_request,
};
use tracedecay_global_db::configuration::contracts::ports::ConfigurationControlStore;

pub struct ConfiguredGitHubSourceAccessAuthorityV1<C> {
    configuration: C,
    scope: ResolvedScope,
    expected_locator: LocatorDigest,
}

impl<C> ConfiguredGitHubSourceAccessAuthorityV1<C> {
    pub fn new(
        configuration: C,
        scope: ResolvedScope,
        repository_owner: &str,
        repository_name: &str,
    ) -> Option<Self> {
        let expected_locator = github_source_locator(repository_owner, repository_name)?;
        scope.validate().ok().map(|()| Self {
            configuration,
            scope,
            expected_locator,
        })
    }
}

impl<C> GitHubSourceAccessAuthorityV1 for ConfiguredGitHubSourceAccessAuthorityV1<C>
where
    C: ConfigurationControlStore + Send + Sync,
{
    fn authorize<'a>(
        &'a self,
        context: &'a tracedecay_contracts::RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubProviderLifecycleV1> {
        Box::pin(async move {
            if request.validate().is_err()
                || context.scope() != &self.scope
                || request.scope.project_id != self.scope.project_id
                || request.scope.repository_id != self.scope.repository_id
                || request.scope.worktree_id != self.scope.worktree_id
                || self
                    .scope
                    .reference
                    .as_ref()
                    .map(tracedecay_domain::RefId::as_str)
                    != Some(request.scope.branch_ref.as_str())
            {
                return GitHubProviderLifecycleV1::Denied;
            }
            let Ok(Some(operation)) = feedback_surface_operation("github_review_ingest") else {
                return GitHubProviderLifecycleV1::Unavailable;
            };
            if operation.capability_id().as_str() != GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1 {
                return GitHubProviderLifecycleV1::Unavailable;
            }
            let observed_at = now_micros();
            let authorization = AuthorizationRequest {
                context,
                operation: &operation,
                observed_at,
            };
            match project_source_access_snapshot_for_request(
                &self.configuration,
                &authorization,
                SourceKindV1::GitHub,
            )
            .await
            {
                ProjectSourceAccessOutcome::Allowed(snapshot)
                    if snapshot.scope == self.scope
                        && snapshot.binding.source_locator_digest == self.expected_locator
                        && snapshot.allows(context, &operation, observed_at) =>
                {
                    GitHubProviderLifecycleV1::Ready
                }
                ProjectSourceAccessOutcome::Allowed(_) | ProjectSourceAccessOutcome::Denied(_) => {
                    GitHubProviderLifecycleV1::Denied
                }
            }
        })
    }
}

impl<C> CiSourceAccessAuthorityV1 for ConfiguredGitHubSourceAccessAuthorityV1<C>
where
    C: ConfigurationControlStore + Send + Sync,
{
    fn authorize_ci<'a>(
        &'a self,
        context: &'a tracedecay_contracts::RequestContext,
        scope: &'a tracedecay_domain::feedback::FeedbackScopeV1,
    ) -> FeedbackPortFuture<'a, CiSourceAccessOutcomeV1> {
        Box::pin(async move {
            if scope.validate().is_err()
                || context.scope() != &self.scope
                || scope.project_id != self.scope.project_id
                || scope.repository_id != self.scope.repository_id
                || scope.worktree_id != self.scope.worktree_id
                || self
                    .scope
                    .reference
                    .as_ref()
                    .map(tracedecay_domain::RefId::as_str)
                    != Some(scope.branch_ref.as_str())
            {
                return CiSourceAccessOutcomeV1::Denied;
            }
            let Ok(Some(operation)) = feedback_surface_operation("ci_failure_localize") else {
                return CiSourceAccessOutcomeV1::Unavailable;
            };
            if operation.capability_id().as_str() != CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1 {
                return CiSourceAccessOutcomeV1::Unavailable;
            }
            let observed_at = now_micros();
            let authorization = AuthorizationRequest {
                context,
                operation: &operation,
                observed_at,
            };
            match project_source_access_snapshot_for_request(
                &self.configuration,
                &authorization,
                SourceKindV1::GitHub,
            )
            .await
            {
                ProjectSourceAccessOutcome::Allowed(snapshot)
                    if snapshot.scope == self.scope
                        && snapshot.binding.source_locator_digest == self.expected_locator
                        && snapshot.allows(context, &operation, observed_at) =>
                {
                    CiSourceAccessOutcomeV1::Ready
                }
                ProjectSourceAccessOutcome::Allowed(_) | ProjectSourceAccessOutcome::Denied(_) => {
                    CiSourceAccessOutcomeV1::Denied
                }
            }
        })
    }
}

/// Identifier of the one daemon-owned GitHub binding derived from a
/// checkout's `origin` remote.
pub const DAEMON_GITHUB_ORIGIN_SOURCE_BINDING_ID: &str = "binding.tracedecay-daemon.github-origin";

fn github_source_locator(repository_owner: &str, repository_name: &str) -> Option<LocatorDigest> {
    if repository_owner.is_empty() || repository_name.is_empty() {
        return None;
    }
    let digest = canonical_sha256(&(
        "tracedecay.advisory.github.source-locator.v1",
        repository_owner,
        repository_name,
    ))
    .ok()?;
    LocatorDigest::new(digest.as_str()).ok()
}

/// The daemon-owned GitHub source binding for `owner/repository`, the
/// repository GitHub reads for this project are authorized against.
pub fn daemon_owned_github_source_binding_v1(
    project_id: &ProjectId,
    repository_owner: &str,
    repository_name: &str,
) -> Option<ScopeSourceBinding> {
    ScopeSourceBinding::new(
        SourceBindingId::new(DAEMON_GITHUB_ORIGIN_SOURCE_BINDING_ID).ok()?,
        SourceKindV1::GitHub,
        github_source_locator(repository_owner, repository_name)?,
        AuthorityRef::Project(project_id.clone()),
    )
    .ok()
}

/// `(owner, repository)` of a `github.com` remote URL, or `None` for any
/// other host, credential-bearing URL, or path shape.
pub fn github_repository_from_remote_v1(remote: &str) -> Option<(String, String)> {
    let (owner, repository) = if let Ok(url) = url::Url::parse(remote) {
        if (url.scheme() != "https" && url.scheme() != "ssh")
            || !url.host_str()?.eq_ignore_ascii_case("github.com")
            || url.password().is_some()
            || (url.scheme() == "https" && !url.username().is_empty())
            || (url.scheme() == "ssh" && url.username() != "git")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let segments = url.path_segments()?.collect::<Vec<_>>();
        if segments.len() != 2 {
            return None;
        }
        (segments[0].to_owned(), segments[1].to_owned())
    } else {
        let remote = remote.strip_prefix("git@github.com:")?;
        let mut segments = remote.split('/');
        let owner = segments.next()?;
        let repository = segments.next()?;
        if segments.next().is_some() {
            return None;
        }
        (owner.to_owned(), repository.to_owned())
    };
    let repository = repository
        .strip_suffix(".git")
        .unwrap_or(&repository)
        .to_owned();
    let target = GitHubRepositoryTargetV1 {
        owner,
        repository,
        pull_request_number: 1,
        pull_request_id: GitHubPullRequestIdV1::new("1").ok()?,
    };
    target
        .validate()
        .then_some((target.owner, target.repository))
}
