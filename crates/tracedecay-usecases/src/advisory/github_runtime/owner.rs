use std::sync::Arc;

use tracedecay_application::feedback::{FeedbackPortFuture, GitHubReviewReadRequestV1};
use tracedecay_application::{RequestContext, ResolvedScope};
use tracedecay_domain::RetrievalAnchorId;
use tracedecay_domain::feedback::{FeedbackScopeV1, GitHubReviewReadOperationV1};

use super::decoder::{
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubOfficialResponseDecoderV1,
    GitHubReviewProviderIdentityV1,
};
use super::network::{
    GitHubHttpReadConfigV1, GitHubReadOnlyClientV1, GitHubReadOnlyCredentialV1,
    GitHubRepositoryTargetV1,
};
use super::store::ProjectGitHubReviewStoreV1;
use super::{
    GitHubReadOnlyRuntimeTransportV1, GitHubReviewBodyEvidenceAuthorityV1,
    GitHubReviewBodyReadOutcomeV1, GitHubReviewRefreshCoordinatorV1, GitHubReviewRefreshOutcomeV1,
    GitHubSourceAccessAuthorityV1,
};
use crate::advisory::{
    GitHubCurrentBranchRemapper, GitHubReadOnlyAdmissionError, GitHubReadOnlyConnector,
    GitHubReadOnlyDescriptorSetV1, GitHubRestDescriptorV1,
};
use tracedecay_runtime_core::db::Database;

pub struct GitHubReviewRuntimeOwnerConfigV1 {
    pub database: Database,
    pub resolved_scope: ResolvedScope,
    pub feedback_scope: FeedbackScopeV1,
    pub target: GitHubRepositoryTargetV1,
    pub credential: GitHubReadOnlyCredentialV1,
    pub http: GitHubHttpReadConfigV1,
    pub identity: GitHubReviewProviderIdentityV1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubReviewRuntimeOwnerBuildErrorV1 {
    InvalidDescriptor,
    InvalidScope,
    InvalidNetworkConfiguration,
    InvalidDecoderConfiguration,
    StoreUnavailable,
}

type RuntimeTransportV1<A> = GitHubReadOnlyRuntimeTransportV1<
    ProjectGitHubReviewStoreV1,
    GitHubReadOnlyClientV1,
    GitHubOfficialResponseDecoderV1<A>,
>;

type RuntimePortV1<R, A> = GitHubReadOnlyConnector<RuntimeTransportV1<A>, R>;

pub struct GitHubReviewRuntimeOwnerV1<R, A> {
    coordinator: GitHubReviewRefreshCoordinatorV1<
        RuntimePortV1<R, A>,
        ProjectGitHubReviewStoreV1,
        Arc<dyn GitHubSourceAccessAuthorityV1>,
    >,
    anchors: A,
    source_access: Arc<dyn GitHubSourceAccessAuthorityV1>,
}

impl<R, A> GitHubReviewRuntimeOwnerV1<R, A>
where
    R: GitHubCurrentBranchRemapper + Sync,
    A: GitHubCanonicalReviewAnchorAuthorityV1 + Sync,
{
    pub fn refresh<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshOutcomeV1> {
        self.coordinator.refresh(context, request)
    }

    pub fn authorize<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, super::GitHubProviderLifecycleV1> {
        self.source_access.authorize(context, request)
    }

    pub fn expand_retained_body<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
        body_anchor: &'a RetrievalAnchorId,
    ) -> FeedbackPortFuture<'a, GitHubReviewBodyReadOutcomeV1>
    where
        A: GitHubReviewBodyEvidenceAuthorityV1,
    {
        self.anchors
            .read_retained_body(context, request, body_anchor, self.source_access.as_ref())
    }
}

pub fn build_github_review_runtime_owner_v1<R, A>(
    config: GitHubReviewRuntimeOwnerConfigV1,
    remapper: R,
    anchors: A,
    source_access: Arc<dyn GitHubSourceAccessAuthorityV1>,
) -> Result<GitHubReviewRuntimeOwnerV1<R, A>, GitHubReviewRuntimeOwnerBuildErrorV1>
where
    R: GitHubCurrentBranchRemapper + Sync,
    A: GitHubCanonicalReviewAnchorAuthorityV1 + Clone + Sync,
{
    if !scope_matches(&config.resolved_scope, &config.feedback_scope) {
        return Err(GitHubReviewRuntimeOwnerBuildErrorV1::InvalidScope);
    }
    let descriptors = GitHubReadOnlyDescriptorSetV1::new(vec![
        rest_descriptor(GitHubReviewReadOperationV1::RestGetPullRequest),
        rest_descriptor(GitHubReviewReadOperationV1::RestListPullRequestReviews),
        rest_descriptor(GitHubReviewReadOperationV1::RestListPullRequestReviewComments),
    ])
    .map_err(map_admission_error)?;
    let store = ProjectGitHubReviewStoreV1::new(config.database, config.feedback_scope)
        .ok_or(GitHubReviewRuntimeOwnerBuildErrorV1::StoreUnavailable)?;
    let client = GitHubReadOnlyClientV1::new(config.target, config.credential, config.http)
        .ok_or(GitHubReviewRuntimeOwnerBuildErrorV1::InvalidNetworkConfiguration)?;
    let decoder = GitHubOfficialResponseDecoderV1::new(config.identity, anchors.clone())
        .ok_or(GitHubReviewRuntimeOwnerBuildErrorV1::InvalidDecoderConfiguration)?;
    let transport = GitHubReadOnlyRuntimeTransportV1::new(store.clone(), client.clone(), decoder);
    let connector = GitHubReadOnlyConnector::new(descriptors, transport, remapper)
        .map_err(map_admission_error)?;
    Ok(GitHubReviewRuntimeOwnerV1 {
        coordinator: GitHubReviewRefreshCoordinatorV1::new(
            connector,
            store,
            Arc::clone(&source_access),
        ),
        anchors,
        source_access,
    })
}

fn rest_descriptor(operation: GitHubReviewReadOperationV1) -> GitHubRestDescriptorV1 {
    GitHubRestDescriptorV1 { operation }
}

fn scope_matches(scope: &ResolvedScope, feedback: &FeedbackScopeV1) -> bool {
    scope.validate().is_ok()
        && feedback.validate().is_ok()
        && scope.project_id == feedback.project_id
        && scope.repository_id == feedback.repository_id
        && scope.worktree_id == feedback.worktree_id
        && scope
            .reference
            .as_ref()
            .map(tracedecay_domain::RefId::as_str)
            == Some(feedback.branch_ref.as_str())
}

fn map_admission_error(
    _error: GitHubReadOnlyAdmissionError,
) -> GitHubReviewRuntimeOwnerBuildErrorV1 {
    GitHubReviewRuntimeOwnerBuildErrorV1::InvalidDescriptor
}
