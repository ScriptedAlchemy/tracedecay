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
use crate::observability::{
    BoundedObservabilityProducerV1, GitHubStackCapabilityObservationResultV1,
    GitHubStackDriftObservationResultV1, GitHubStackProbeOwnerV1, record_github_stack_capability,
    record_github_stack_drifts,
};
use crate::stack_coordinator::{
    DaemonGitHubStackCoordinatorV1, GitHubStackObservationV1, GitHubStackProviderSourceBindingV1,
};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::Database;

/// Canonical Observatory mount for the coordinator observations this owner
/// produces. Absent when the composition root could not mount the probe
/// owner, producer, and observation database; refresh then keeps producing
/// product anchors without canonical capability/drift receipts.
#[derive(Clone)]
pub struct GitHubStackObservabilityV1 {
    pub probe_owner: GitHubStackProbeOwnerV1,
    pub producer: Arc<BoundedObservabilityProducerV1>,
    pub observation_db: RegisteredGlobalDbLeaseV1,
}

impl GitHubStackObservabilityV1 {
    /// Offers one validated coordinator observation as a capability receipt
    /// plus one receipt per exact drift interval. Telemetry refusal is
    /// logged, never propagated to the refresh product path.
    pub fn record(
        &self,
        source_binding: &GitHubStackProviderSourceBindingV1,
        observation: &GitHubStackObservationV1,
    ) {
        let capability = record_github_stack_capability(
            self.observation_db.as_ref(),
            Some(self.producer.as_ref()),
            &self.probe_owner,
            source_binding,
            observation,
        );
        if capability != GitHubStackCapabilityObservationResultV1::Enqueued {
            tracing::warn!(
                event = "github_stack_capability_observation_refused",
                outcome = ?capability,
                "GitHub stack capability receipt did not enter the canonical producer"
            );
        }
        match record_github_stack_drifts(
            self.observation_db.as_ref(),
            Some(self.producer.as_ref()),
            &self.probe_owner,
            source_binding,
            observation,
        ) {
            GitHubStackDriftObservationResultV1::Emitted { dropped: 0, .. } => {}
            refused => {
                tracing::warn!(
                    event = "github_stack_drift_observation_refused",
                    outcome = ?refused,
                    "GitHub stack drift receipts did not fully enter the canonical producer"
                );
            }
        }
    }
}

pub struct GitHubReviewRuntimeOwnerConfigV1 {
    pub database: Database,
    pub resolved_scope: ResolvedScope,
    pub feedback_scope: FeedbackScopeV1,
    pub target: GitHubRepositoryTargetV1,
    pub credential: GitHubReadOnlyCredentialV1,
    pub http: GitHubHttpReadConfigV1,
    pub identity: GitHubReviewProviderIdentityV1,
    pub stack_coordinator: Arc<DaemonGitHubStackCoordinatorV1>,
    pub stack_anchor_db: RegisteredGlobalDbLeaseV1,
    pub stack_observability: Option<GitHubStackObservabilityV1>,
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
    stack_client: GitHubReadOnlyClientV1,
    stack_scope: ResolvedScope,
    stack_provider: tracedecay_domain::ProviderId,
    stack_coordinator: Arc<DaemonGitHubStackCoordinatorV1>,
    stack_anchors: super::ProjectGitHubStackAnchorAuthorityV1,
    stack_observability: Option<GitHubStackObservabilityV1>,
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
        Box::pin(async move {
            let outcome = self.coordinator.refresh(context, request).await;
            let stack_read_enabled = match self
                .stack_coordinator
                .should_read_provider_stack(&self.stack_scope)
            {
                Ok(enabled) => enabled,
                Err(error) => {
                    tracing::warn!(
                        event = "github_stack_policy_read_failed",
                        error = ?error,
                        "GitHub stack policy observation failed"
                    );
                    false
                }
            };
            if matches!(outcome, GitHubReviewRefreshOutcomeV1::Stored(_)) && stack_read_enabled {
                let client = self.stack_client.clone();
                let stack_context = context.clone();
                let stack_review_request = request.clone();
                let stack_provider = self.stack_provider.clone();
                let stack_anchors = self.stack_anchors.clone();
                let stack_request = super::GitHubGraphQlReadRequestV1 {
                    scope: request.scope.clone(),
                    pull_request_id: request.pull_request_id.clone(),
                    resume: super::GitHubReadResumeV1::empty(),
                };
                let provider_outcome = match tokio::task::spawn_blocking(move || {
                    client.read_stack(
                        &stack_context,
                        &stack_request,
                        &stack_review_request,
                        &stack_provider,
                        &stack_anchors,
                    )
                })
                .await
                {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        tracing::warn!(
                            event = "github_stack_read_task_failed",
                            error = %error,
                            "GitHub stack read task failed"
                        );
                        crate::stack_coordinator::GitHubStackProviderOutcomeV1::Unavailable
                    }
                };
                let stack_observed_at = tracedecay_application::now_micros();
                let Some(source_binding) = self.stack_anchors.source_binding(
                    context,
                    request,
                    &provider_outcome,
                    stack_observed_at,
                ) else {
                    tracing::warn!(
                        event = "github_stack_source_binding_failed",
                        "GitHub stack evidence could not be bound to its exact source owner"
                    );
                    return outcome;
                };
                match self.stack_coordinator.observe_provider(
                    self.stack_scope.clone(),
                    self.stack_provider.clone(),
                    provider_outcome,
                    source_binding.clone(),
                    stack_observed_at,
                ) {
                    Ok(observation) => {
                        // Offered before anchor publication so a publication
                        // refusal below cannot conceal the observation the
                        // coordinator already made.
                        if let Some(stack_observability) = &self.stack_observability {
                            stack_observability.record(&source_binding, &observation);
                        }
                        let anchor_publication = self
                            .stack_anchors
                            .publish(context, request, &observation, self.source_access.as_ref())
                            .await;
                        if !matches!(
                            anchor_publication,
                            super::GitHubStackAnchorPublicationOutcomeV1::Published
                                | super::GitHubStackAnchorPublicationOutcomeV1::Replayed
                        ) {
                            tracing::warn!(
                                event = "github_stack_anchor_publication_failed",
                                outcome = ?anchor_publication,
                                "GitHub stack evidence was not published to the retrieval-anchor authority"
                            );
                            return outcome;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            event = "github_stack_provider_observation_failed",
                            error = ?error,
                            "GitHub stack provider observation failed"
                        );
                    }
                }
            }
            outcome
        })
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
    let stack_scope = config.resolved_scope.clone();
    let stack_provider = config.identity.provider.clone();
    let stack_coordinator = Arc::clone(&config.stack_coordinator);
    let stack_observability = config.stack_observability.clone();
    let stack_anchors = super::ProjectGitHubStackAnchorAuthorityV1::new(
        config.stack_anchor_db.clone(),
        config.feedback_scope.clone(),
    )
    .ok_or(GitHubReviewRuntimeOwnerBuildErrorV1::StoreUnavailable)?;
    let store = ProjectGitHubReviewStoreV1::new(config.database, config.feedback_scope)
        .ok_or(GitHubReviewRuntimeOwnerBuildErrorV1::StoreUnavailable)?;
    let client = GitHubReadOnlyClientV1::new(config.target, config.credential, config.http)
        .ok_or(GitHubReviewRuntimeOwnerBuildErrorV1::InvalidNetworkConfiguration)?;
    let decoder = GitHubOfficialResponseDecoderV1::new(config.identity, anchors.clone())
        .ok_or(GitHubReviewRuntimeOwnerBuildErrorV1::InvalidDecoderConfiguration)?;
    let stack_client = client.clone();
    let transport = GitHubReadOnlyRuntimeTransportV1::new(store.clone(), client, decoder);
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
        stack_client,
        stack_scope,
        stack_provider,
        stack_coordinator,
        stack_anchors,
        stack_observability,
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
