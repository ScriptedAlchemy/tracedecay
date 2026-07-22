#![allow(dead_code)] // in-flight feature APIs not yet wired; see clippy sweep
use tracedecay_application::feedback::{FeedbackPortFuture, GitHubReviewReadRequestV1};
use tracedecay_application::{
    AuthorizationPortOutcome, RequestContext, ResolvedScope, SourceAuthorizationSnapshot,
};
use tracedecay_domain::feedback::{FeedbackScopeV1, GitHubReviewReadOperationV1};
use tracedecay_policy::authorization::{
    AuthorizationSnapshotStateV1, SinkKindV1, SourceOwnerV1, TypedOperationV1,
};

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
    GitHubReadOnlyRuntimeTransportV1, GitHubReviewRefreshCoordinatorV1,
    GitHubReviewRefreshOutcomeV1,
};
use crate::application::advisory::{
    GitHubCurrentBranchRemapper, GitHubReadOnlyAdmissionError, GitHubReadOnlyConnector,
    GitHubReadOnlyDescriptorSetV1, GitHubRestDescriptorV1,
};
use crate::db::Database;

pub struct GitHubReviewRuntimeOwnerConfigV1 {
    pub database: Database,
    pub resolved_scope: ResolvedScope,
    pub feedback_scope: FeedbackScopeV1,
    pub source_authorization: AuthorizationPortOutcome,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubProviderLifecycleV1 {
    Ready,
    Denied,
    Stale,
    Ambiguous,
    Unavailable,
}

struct GitHubSourceAccessV1 {
    scope: ResolvedScope,
    snapshot: Option<SourceAuthorizationSnapshot>,
    lifecycle: GitHubProviderLifecycleV1,
}

impl GitHubSourceAccessV1 {
    fn new(scope: ResolvedScope, outcome: AuthorizationPortOutcome) -> Self {
        let (snapshot, lifecycle) = match outcome {
            AuthorizationPortOutcome::Snapshot(snapshot) => {
                let input = snapshot.input();
                let lifecycle = match input.snapshot_state {
                    AuthorizationSnapshotStateV1::Complete
                        if input.requested_access.operation == TypedOperationV1::ProviderFetch
                            && input.requested_access.sink == SinkKindV1::ProviderFetch
                            && input.resolved_owner_scope.owner
                                == SourceOwnerV1::Project(scope.project_id.clone()) =>
                    {
                        GitHubProviderLifecycleV1::Ready
                    }
                    AuthorizationSnapshotStateV1::Stale => GitHubProviderLifecycleV1::Stale,
                    AuthorizationSnapshotStateV1::Ambiguous => GitHubProviderLifecycleV1::Ambiguous,
                    AuthorizationSnapshotStateV1::Missing
                    | AuthorizationSnapshotStateV1::Partial
                    | AuthorizationSnapshotStateV1::Complete => GitHubProviderLifecycleV1::Denied,
                };
                (
                    (lifecycle == GitHubProviderLifecycleV1::Ready).then_some(*snapshot),
                    lifecycle,
                )
            }
            AuthorizationPortOutcome::Absent => (None, GitHubProviderLifecycleV1::Denied),
            AuthorizationPortOutcome::Stale(_) => (None, GitHubProviderLifecycleV1::Stale),
            AuthorizationPortOutcome::Unavailable(_) => {
                (None, GitHubProviderLifecycleV1::Unavailable)
            }
        };
        Self {
            scope,
            snapshot,
            lifecycle,
        }
    }

    fn allows(&self, context: &RequestContext) -> bool {
        self.lifecycle == GitHubProviderLifecycleV1::Ready
            && context.validate().is_ok()
            && context.scope() == &self.scope
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| context.actor() == &snapshot.input().requester)
    }
}

type RuntimeTransportV1<A> = GitHubReadOnlyRuntimeTransportV1<
    ProjectGitHubReviewStoreV1,
    GitHubReadOnlyClientV1,
    GitHubOfficialResponseDecoderV1<A>,
>;

type RuntimePortV1<R, A> = GitHubReadOnlyConnector<RuntimeTransportV1<A>, R>;

pub struct GitHubReviewRuntimeOwnerV1<R, A> {
    coordinator: GitHubReviewRefreshCoordinatorV1<RuntimePortV1<R, A>, ProjectGitHubReviewStoreV1>,
    source_access: GitHubSourceAccessV1,
    client: GitHubReadOnlyClientV1,
}

impl<R, A> GitHubReviewRuntimeOwnerV1<R, A>
where
    R: GitHubCurrentBranchRemapper + Sync,
    A: GitHubCanonicalReviewAnchorAuthorityV1 + Sync,
{
    pub fn provider_lifecycle(&self) -> GitHubProviderLifecycleV1 {
        self.source_access.lifecycle
    }

    pub(crate) fn ci_client(&self, context: &RequestContext) -> Option<&GitHubReadOnlyClientV1> {
        self.source_access.allows(context).then_some(&self.client)
    }

    pub fn refresh<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshOutcomeV1> {
        if !self.source_access.allows(context) {
            return Box::pin(async { GitHubReviewRefreshOutcomeV1::Denied });
        }
        self.coordinator.refresh(context, request)
    }
}

pub fn build_github_review_runtime_owner_v1<R, A>(
    config: GitHubReviewRuntimeOwnerConfigV1,
    remapper: R,
    anchors: A,
) -> Result<GitHubReviewRuntimeOwnerV1<R, A>, GitHubReviewRuntimeOwnerBuildErrorV1>
where
    R: GitHubCurrentBranchRemapper + Sync,
    A: GitHubCanonicalReviewAnchorAuthorityV1 + Sync,
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
    let decoder = GitHubOfficialResponseDecoderV1::new(config.identity, anchors)
        .ok_or(GitHubReviewRuntimeOwnerBuildErrorV1::InvalidDecoderConfiguration)?;
    let transport = GitHubReadOnlyRuntimeTransportV1::new(store.clone(), client.clone(), decoder);
    let connector = GitHubReadOnlyConnector::new(descriptors, transport, remapper)
        .map_err(map_admission_error)?;
    Ok(GitHubReviewRuntimeOwnerV1 {
        coordinator: GitHubReviewRefreshCoordinatorV1::new(connector, store),
        source_access: GitHubSourceAccessV1::new(
            config.resolved_scope,
            config.source_authorization,
        ),
        client,
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
