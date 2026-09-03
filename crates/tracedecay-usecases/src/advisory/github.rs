//! Structurally read-only GitHub review-ingress adapter.

use serde::{Deserialize, Serialize};
use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    FeedbackPortFuture, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
    GitHubReviewReadPort, GitHubReviewReadPortOutcomeV1, GitHubReviewReadRequestV1,
    GitHubReviewReadResponseV1,
};
use tracedecay_domain::feedback::{
    FeedbackScopeV1, GitHubReviewCurrentBranchRemapV1, GitHubReviewLifecycleV1,
    GitHubReviewReadOperationV1, GitHubReviewRemapStateV1,
};

use super::context_allows_feedback_operation;

/// A fixed ingress descriptor, deliberately lacking a URL, body, or method.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubRestDescriptorV1 {
    pub operation: GitHubReviewReadOperationV1,
}

impl GitHubRestDescriptorV1 {
    pub fn validate(&self) -> Result<(), GitHubReadOnlyAdmissionError> {
        self.operation
            .is_rest()
            .then_some(())
            .ok_or(GitHubReadOnlyAdmissionError::InvalidRestDescriptor)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubReadOnlyAdmissionError {
    InvalidRestDescriptor,
    DuplicateRestDescriptor,
}

/// Closed REST descriptor set. The sole GraphQL operation is represented by
/// [`GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads`] and
/// its query text is private compile-time client code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHubReadOnlyDescriptorSetV1 {
    rest: Vec<GitHubRestDescriptorV1>,
}

impl GitHubReadOnlyDescriptorSetV1 {
    pub fn new(rest: Vec<GitHubRestDescriptorV1>) -> Result<Self, GitHubReadOnlyAdmissionError> {
        let value = Self { rest };
        value.scan()?;
        Ok(value)
    }

    pub fn scan(&self) -> Result<(), GitHubReadOnlyAdmissionError> {
        for (index, descriptor) in self.rest.iter().enumerate() {
            descriptor.validate()?;
            if self.rest[index.saturating_add(1)..]
                .iter()
                .any(|other| other.operation == descriptor.operation)
            {
                return Err(GitHubReadOnlyAdmissionError::DuplicateRestDescriptor);
            }
        }
        Ok(())
    }

    pub fn rest_descriptor(
        &self,
        operation: GitHubReviewReadOperationV1,
    ) -> Result<GitHubRestDescriptorV1, GitHubReadOnlyAdmissionError> {
        self.scan()?;
        self.rest
            .iter()
            .copied()
            .find(|descriptor| descriptor.operation == operation)
            .ok_or(GitHubReadOnlyAdmissionError::InvalidRestDescriptor)
    }
}

/// The only network-shaped methods: fixed GET descriptor or normalized query.
pub trait GitHubReadOnlyTransport {
    fn rest_get<'a>(
        &'a self,
        context: &'a RequestContext,
        descriptor: GitHubRestDescriptorV1,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1>;

    fn graphql_query<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1>;
}

/// Exact anchor remapping is injected; `None` forbids a guessed current line.
pub trait GitHubCurrentBranchRemapper {
    fn remap<'a>(
        &'a self,
        context: &'a RequestContext,
        current_scope: &'a FeedbackScopeV1,
        original: &'a tracedecay_domain::feedback::GitHubReviewImmutableAnchorV1,
    ) -> FeedbackPortFuture<'a, Option<GitHubReviewCurrentBranchRemapV1>>;
}

pub struct GitHubReadOnlyConnector<T, R> {
    descriptors: GitHubReadOnlyDescriptorSetV1,
    transport: T,
    remapper: R,
}

impl<T, R> GitHubReadOnlyConnector<T, R> {
    pub fn new(
        descriptors: GitHubReadOnlyDescriptorSetV1,
        transport: T,
        remapper: R,
    ) -> Result<Self, GitHubReadOnlyAdmissionError> {
        descriptors.scan()?;
        Ok(Self {
            descriptors,
            transport,
            remapper,
        })
    }

    async fn normalize_remaps(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
        response: &mut GitHubReviewReadResponseV1,
    ) -> bool
    where
        R: GitHubCurrentBranchRemapper,
    {
        for item in &mut response.ingress.items {
            let original = item.remap.original.clone();
            let remap = match self
                .remapper
                .remap(context, &request.scope, &original)
                .await
            {
                Some(remap) => remap,
                None => match GitHubReviewCurrentBranchRemapV1::unmapped(
                    original.clone(),
                    request.scope.clone(),
                ) {
                    Ok(remap) => remap,
                    Err(_) => return false,
                },
            };
            if remap.validate().is_err()
                || remap.current_scope != request.scope
                || remap.original != original
            {
                return false;
            }
            item.remap = remap;
            if item.lifecycle == GitHubReviewLifecycleV1::Current
                && item.remap.state != GitHubReviewRemapStateV1::ExactCurrent
            {
                item.lifecycle = GitHubReviewLifecycleV1::Outdated;
            }
        }
        response.validate_for(request).is_ok()
    }
}

impl<T, R> GitHubReviewReadPort for GitHubReadOnlyConnector<T, R>
where
    T: GitHubReadOnlyTransport + Sync,
    R: GitHubCurrentBranchRemapper + Sync,
{
    fn read<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1> {
        Box::pin(async move {
            if request.validate().is_err() || self.descriptors.scan().is_err() {
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
            let rest_descriptor = if request.operation.is_rest() {
                let Ok(descriptor) = self.descriptors.rest_descriptor(request.operation) else {
                    return GitHubReviewReadPortOutcomeV1::Unavailable;
                };
                Some(descriptor)
            } else {
                None
            };
            let graphql_query = request.operation.is_graphql_query();
            if rest_descriptor.is_none() && !graphql_query {
                return GitHubReviewReadPortOutcomeV1::Unavailable;
            }
            let outcome = if let Some(descriptor) = rest_descriptor {
                self.transport.rest_get(context, descriptor, request).await
            } else if graphql_query {
                self.transport.graphql_query(context, request).await
            } else {
                return GitHubReviewReadPortOutcomeV1::Unavailable;
            };
            match outcome {
                GitHubReviewReadPortOutcomeV1::Read(mut response) => {
                    if self.normalize_remaps(context, request, &mut response).await {
                        GitHubReviewReadPortOutcomeV1::Read(response)
                    } else {
                        GitHubReviewReadPortOutcomeV1::Unavailable
                    }
                }
                other => other,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::{
        ActorId, CommitId, ManifestDigest, ProjectId, RefId, RepositoryId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;

    const SHA: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[derive(Default)]
    struct TransportCalls {
        rest: AtomicUsize,
        graphql: AtomicUsize,
    }

    struct TransportProbe(Arc<TransportCalls>);

    impl GitHubReadOnlyTransport for TransportProbe {
        fn rest_get<'a>(
            &'a self,
            _context: &'a RequestContext,
            _descriptor: GitHubRestDescriptorV1,
            _request: &'a GitHubReviewReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1> {
            self.0.rest.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { GitHubReviewReadPortOutcomeV1::Unavailable })
        }

        fn graphql_query<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a GitHubReviewReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubReviewReadPortOutcomeV1> {
            self.0.graphql.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { GitHubReviewReadPortOutcomeV1::Unavailable })
        }
    }

    struct NoRemap;

    impl GitHubCurrentBranchRemapper for NoRemap {
        fn remap<'a>(
            &'a self,
            _context: &'a RequestContext,
            _current_scope: &'a FeedbackScopeV1,
            _original: &'a tracedecay_domain::feedback::GitHubReviewImmutableAnchorV1,
        ) -> FeedbackPortFuture<'a, Option<GitHubReviewCurrentBranchRemapV1>> {
            Box::pin(async { None })
        }
    }

    fn context_and_scope_with_grant(
        capability: &str,
        use_case: &str,
    ) -> (RequestContext, FeedbackScopeV1) {
        let project_id = ProjectId::new("project.github-advisory").unwrap();
        let repository_id = RepositoryId::new("repository.github-advisory").unwrap();
        let worktree_id = WorktreeId::new("worktree.github-advisory").unwrap();
        let reference = RefId::new("refs/heads/github-advisory").unwrap();
        let resolved_scope = ResolvedScope::new(
            project_id.clone(),
            repository_id.clone(),
            worktree_id.clone(),
            Some(reference),
        )
        .unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.github-advisory").unwrap(),
            1,
            ManifestDigest::new(SHA).unwrap(),
            ActorId::new("actor.github-advisory-issuer").unwrap(),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            resolved_scope.clone(),
            BTreeSet::from([CapabilityId::new(capability).unwrap()]),
            BTreeSet::from([UseCaseId::new(use_case).unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        let context = RequestContext::new(
            ActorId::new("actor.github-advisory").unwrap(),
            resolved_scope,
            grant,
            RequestId::new("request.github-advisory").unwrap(),
            Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
            CancellationContext::active("cancel.github-advisory").unwrap(),
        )
        .unwrap();
        let scope = FeedbackScopeV1 {
            project_id,
            repository_id,
            worktree_id,
            branch_ref: "refs/heads/github-advisory".to_owned(),
            head_commit_id: CommitId::new("commit.github-advisory").unwrap(),
        };
        (context, scope)
    }

    fn context_and_scope() -> (RequestContext, FeedbackScopeV1) {
        context_and_scope_with_grant(
            GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        )
    }

    fn request(
        scope: FeedbackScopeV1,
        operation: GitHubReviewReadOperationV1,
    ) -> GitHubReviewReadRequestV1 {
        GitHubReviewReadRequestV1 {
            operation,
            scope,
            pull_request_id: tracedecay_domain::feedback::GitHubPullRequestIdV1::new(
                "pull-request.github-advisory",
            )
            .unwrap(),
        }
    }

    #[test]
    fn descriptor_scan_rejects_duplicates() {
        let descriptor = GitHubRestDescriptorV1 {
            operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
        };
        assert!(GitHubReadOnlyDescriptorSetV1::new(vec![descriptor, descriptor]).is_err());
    }

    #[tokio::test]
    async fn admission_routes_only_fixed_get_or_static_query() {
        let (context, scope) = context_and_scope();
        let transport_calls = Arc::new(TransportCalls::default());
        let connector = GitHubReadOnlyConnector::new(
            GitHubReadOnlyDescriptorSetV1::new(Vec::new()).unwrap(),
            TransportProbe(Arc::clone(&transport_calls)),
            NoRemap,
        )
        .unwrap();
        let outcome = connector
            .read(
                &context,
                &request(
                    scope.clone(),
                    GitHubReviewReadOperationV1::RestListPullRequestReviews,
                ),
            )
            .await;
        assert_eq!(outcome, GitHubReviewReadPortOutcomeV1::Unavailable);
        assert_eq!(transport_calls.rest.load(Ordering::SeqCst), 0);
        assert_eq!(transport_calls.graphql.load(Ordering::SeqCst), 0);

        let (unauthorized_context, unauthorized_scope) = context_and_scope_with_grant(
            "capability.application.feedback.diagnostics",
            "use-case.application.feedback.diagnostics",
        );
        let transport_calls = Arc::new(TransportCalls::default());
        let connector = GitHubReadOnlyConnector::new(
            GitHubReadOnlyDescriptorSetV1::new(vec![GitHubRestDescriptorV1 {
                operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
            }])
            .unwrap(),
            TransportProbe(Arc::clone(&transport_calls)),
            NoRemap,
        )
        .unwrap();
        let outcome = connector
            .read(
                &unauthorized_context,
                &request(
                    unauthorized_scope,
                    GitHubReviewReadOperationV1::RestListPullRequestReviews,
                ),
            )
            .await;
        assert_eq!(outcome, GitHubReviewReadPortOutcomeV1::Denied);
        assert_eq!(transport_calls.rest.load(Ordering::SeqCst), 0);
        assert_eq!(transport_calls.graphql.load(Ordering::SeqCst), 0);

        let transport_calls = Arc::new(TransportCalls::default());
        let connector = GitHubReadOnlyConnector::new(
            GitHubReadOnlyDescriptorSetV1::new(Vec::new()).unwrap(),
            TransportProbe(Arc::clone(&transport_calls)),
            NoRemap,
        )
        .unwrap();
        let outcome = connector
            .read(
                &context,
                &request(
                    scope,
                    GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
                ),
            )
            .await;
        assert_eq!(outcome, GitHubReviewReadPortOutcomeV1::Unavailable);
        assert_eq!(transport_calls.rest.load(Ordering::SeqCst), 0);
        assert_eq!(transport_calls.graphql.load(Ordering::SeqCst), 1);
    }
}
