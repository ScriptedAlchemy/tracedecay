use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, FeedbackPortFuture,
    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GitHubReviewReadRequestV1, feedback_surface_operation,
};
use tracedecay_application::{AuthorizationPhase, AuthorizationRequest, ResolvedScope, now_micros};
use tracedecay_domain::configuration::SourceKindV1;
use tracedecay_domain::{LocatorDigest, canonical_sha256};

use super::{GitHubProviderLifecycleV1, GitHubSourceAccessAuthorityV1};
use crate::advisory::ci_runtime::{
    CiSourceAccessAuthorityV1, CiSourceAccessOutcomeV1,
};
use crate::configuration::ConfigurationControlStore;
use crate::source_authorization::{
    ProjectSourceAccessOutcome, project_source_access_snapshot_for_request,
};

pub(crate) struct ConfiguredGitHubSourceAccessAuthorityV1<C> {
    configuration: C,
    scope: ResolvedScope,
    expected_locator: LocatorDigest,
}

impl<C> ConfiguredGitHubSourceAccessAuthorityV1<C> {
    pub(crate) fn new(
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
        context: &'a tracedecay_application::RequestContext,
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
                phase: AuthorizationPhase::Admission,
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
        context: &'a tracedecay_application::RequestContext,
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
                phase: AuthorizationPhase::Admission,
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

fn github_source_locator(repository_owner: &str, repository_name: &str) -> Option<LocatorDigest> {
    if repository_owner.is_empty() || repository_name.is_empty() {
        return None;
    }
    let digest = canonical_sha256(&(
        "tracedecay.pr13.github.source-locator.v1",
        repository_owner,
        repository_name,
    ))
    .ok()?;
    LocatorDigest::new(digest.as_str()).ok()
}
