use std::sync::Arc;

use tracedecay_contracts::feedback::{FeedbackPortFuture, GitHubReviewReadRequestV1};
use tracedecay_contracts::{RequestContext, ResolvedScope};
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
    GitHubReadOnlyRuntimeTransportV1, GitHubReviewRefreshCoordinatorV1,
    GitHubReviewRefreshOutcomeV1, GitHubSourceAccessAuthorityV1,
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
    DaemonGitHubStackCoordinatorV1, GitHubStackCoordinatorErrorV1, GitHubStackObservationV1,
    GitHubStackProviderOutcomeV1, GitHubStackProviderSourceBindingV1,
};
use tracedecay_global_db::RegisteredGlobalDbLeaseV1;
use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::native_runtime;

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

#[derive(Clone)]
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

pub struct GitHubReviewRuntimeOwnerV1<R, A> {
    coordinator: GitHubReviewRefreshCoordinatorV1<
        GitHubReadOnlyConnector<RuntimeTransportV1<A>, R>,
        ProjectGitHubReviewStoreV1,
        Arc<dyn GitHubSourceAccessAuthorityV1>,
    >,
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
            if !outcome.has_usable_observation()
                || !matches!(
                    request.operation,
                    GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads
                        | GitHubReviewReadOperationV1::RestListPullRequestReviewComments
                )
            {
                return outcome;
            }
            // The read is authorized by the scope's registration before any
            // network I/O; its completion may publish only under that same
            // registration.
            let stack_read = match self
                .stack_coordinator
                .authorize_provider_read(&self.stack_scope)
            {
                Ok(read) => read,
                Err(error) => {
                    tracing::warn!(
                        event = "github_stack_policy_read_failed",
                        error = ?error,
                        "GitHub stack read authorization failed"
                    );
                    return outcome;
                }
            };
            if let Some(stack_read) = stack_read {
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
                let provider_outcome = match native_runtime::spawn_blocking(move || {
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
                let stack_observed_at = tracedecay_contracts::now_micros();
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
                    stack_read,
                    self.stack_provider.clone(),
                    provider_outcome,
                    source_binding.clone(),
                    stack_observed_at,
                ) {
                    Ok(observation) => {
                        self.publish_stack_observation(
                            context,
                            request,
                            &source_binding,
                            &observation,
                        )
                        .await;
                    }
                    Err(GitHubStackCoordinatorErrorV1::Superseded) => {
                        tracing::debug!(
                            event = "github_stack_provider_read_superseded",
                            "GitHub stack read outlived its scope registration and was not published"
                        );
                        self.observe_stack_policy(context, request).await;
                    }
                    Err(error) => {
                        tracing::warn!(
                            event = "github_stack_provider_observation_failed",
                            error = ?error,
                            "GitHub stack provider observation failed"
                        );
                    }
                }
            } else {
                self.observe_stack_policy(context, request).await;
            }
            outcome
        })
    }

    async fn observe_stack_policy(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
    ) {
        // A superseded provider response has no authority over the current
        // policy. Bind a policy-only observation to the current source instead.
        let observed_at = tracedecay_contracts::now_micros();
        let Some(binding) = self.stack_anchors.source_binding(
            context,
            request,
            &GitHubStackProviderOutcomeV1::Unavailable,
            observed_at,
        ) else {
            tracing::warn!(
                event = "github_stack_source_binding_failed",
                "GitHub stack policy could not be bound to its exact source owner"
            );
            return;
        };
        match self.stack_coordinator.observe_policy(
            self.stack_scope.clone(),
            self.stack_provider.clone(),
            binding.clone(),
            observed_at,
        ) {
            Ok(observation) => {
                // The coordinator preserves a provider observation already
                // published by the current registration; it needs no new
                // policy receipt with a different source binding.
                if matches!(
                    observation.capability.state,
                    tracedecay_domain::GitHubStackCapabilityStateV1::PrivatePreviewDisabled
                        | tracedecay_domain::GitHubStackCapabilityStateV1::Unavailable
                ) {
                    self.publish_stack_observation(context, request, &binding, &observation)
                        .await;
                }
            }
            Err(GitHubStackCoordinatorErrorV1::Superseded) => {
                tracing::debug!(
                    event = "github_stack_policy_observation_superseded",
                    "GitHub stack policy changed before its observation was published"
                );
            }
            Err(error) => {
                tracing::warn!(
                    event = "github_stack_policy_observation_failed",
                    error = ?error,
                    "GitHub stack policy observation failed"
                );
            }
        }
    }

    async fn publish_stack_observation(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
        binding: &GitHubStackProviderSourceBindingV1,
        observation: &GitHubStackObservationV1,
    ) {
        // Offered before anchor publication so a publication refusal cannot
        // conceal the observation the coordinator already made.
        if let Some(observability) = &self.stack_observability {
            observability.record(binding, observation);
        }
        let publication = self
            .stack_anchors
            .publish(context, request, observation, self.source_access.as_ref())
            .await;
        if !matches!(
            publication,
            super::GitHubStackAnchorPublicationOutcomeV1::Published
                | super::GitHubStackAnchorPublicationOutcomeV1::Replayed
        ) {
            tracing::warn!(
                event = "github_stack_anchor_publication_failed",
                outcome = ?publication,
                "GitHub stack evidence was not published to the retrieval-anchor authority"
            );
        }
    }

    /// Retained review state for local feedback: no provider or stack read.
    pub fn retained<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, GitHubReviewRefreshOutcomeV1> {
        self.coordinator.retained(context, request)
    }

    pub fn authorize<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
    ) -> FeedbackPortFuture<'a, super::GitHubProviderLifecycleV1> {
        self.source_access.authorize(context, request)
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
        rest_descriptor(GitHubReviewReadOperationV1::RestListPullRequestConversationComments),
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

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::{Arc, mpsc};

    use serde_json::json;
    use tracedecay_contracts::CancellationContext;
    use tracedecay_domain::configuration::GitHubStackedPullRequestPolicyV1;
    use tracedecay_domain::feedback::{
        GitHubReviewCurrentBranchRemapV1, GitHubReviewImmutableAnchorV1,
    };
    use tracedecay_domain::{
        CommitId, GitHubStackCapabilityStateV1, GitTopologyAnchorTargetV1, IntervalStateV1,
        ProviderId, RetrievalAnchorRecord, RetrievalAnchorTarget, UtcMicros,
    };
    use tracedecay_global_db::tests::harness::open_registered_test_database_fixture;
    use tracedecay_runtime_core::db::{
        DatabaseAuthority, TestDatabaseRuntimeMode, TestDatabaseRuntimeScope,
    };

    use super::super::decoder::{GitHubCanonicalReviewAnchorsV1, GitHubReviewAnchorSeedV1};
    use super::super::network::RegisteredGitHubReadOnlyCredentialV1;
    use super::super::network::test_support::{
        local_client, read_http_request_with_headers, write_http_json,
    };
    use super::super::network::tests::{
        FixtureCredentialAuthorityModeV1, context, registered_fixture_credential, request, scope,
    };
    use super::super::{GitHubProviderLifecycleV1, ProjectGitHubStackAnchorAuthorityV1};
    use super::*;

    #[derive(Clone)]
    struct EmptyReviewAnchors;

    impl GitHubCanonicalReviewAnchorAuthorityV1 for EmptyReviewAnchors {
        fn resolve<'a>(
            &'a self,
            _request: &'a GitHubReviewReadRequestV1,
            _seed: &'a GitHubReviewAnchorSeedV1,
        ) -> FeedbackPortFuture<'a, Option<GitHubCanonicalReviewAnchorsV1>> {
            panic!("an empty review collection has no anchors to resolve")
        }
    }

    impl GitHubCurrentBranchRemapper for EmptyReviewAnchors {
        fn remap<'a>(
            &'a self,
            _context: &'a RequestContext,
            _scope: &'a FeedbackScopeV1,
            _original: &'a GitHubReviewImmutableAnchorV1,
        ) -> FeedbackPortFuture<'a, Option<GitHubReviewCurrentBranchRemapV1>> {
            panic!("an empty review collection has no anchors to remap")
        }
    }

    struct ReadySource;

    impl GitHubSourceAccessAuthorityV1 for ReadySource {
        fn authorize<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a GitHubReviewReadRequestV1,
        ) -> FeedbackPortFuture<'a, GitHubProviderLifecycleV1> {
            Box::pin(async { GitHubProviderLifecycleV1::Ready })
        }
    }

    /// Explicit refresh is the only writer of retained review state; local
    /// feedback reads it back from the real store without a provider
    /// connection, and never waits on (or is cancelled by) a held refresh.
    #[tokio::test]
    async fn retained_reads_serve_refreshed_state_without_waiting_on_a_held_refresh() {
        let repository = "runtime-retained-isolation";
        let mut scope = scope(repository);
        scope.head_commit_id = CommitId::new("dddddddddddddddddddddddddddddddddddddddd").unwrap();
        let context = context(&scope);
        let mut request = request(scope.clone());
        request.operation = GitHubReviewReadOperationV1::RestListPullRequestReviewComments;
        let storage = tempfile::tempdir().unwrap();
        let (anchor_database, _anchor_owner) = open_registered_test_database_fixture(
            &storage.path().join("project-sessions.db"),
            TestDatabaseRuntimeScope::ProjectSessions {
                project_id: scope.project_id.clone(),
            },
        )
        .await
        .unwrap();
        crate::register_test_schema_installer();
        let review_path = storage.path().join("review.db");
        let authority =
            DatabaseAuthority::acquire_test(&review_path, "github-runtime-retained").unwrap();
        let (review_database, _) = Database::publish_test_runtime(
            &review_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let coordinator = Arc::new(DaemonGitHubStackCoordinatorV1::default());
        coordinator
            .register_scope(context.scope(), GitHubStackedPullRequestPolicyV1::Disabled)
            .unwrap();
        let (_credential_authority, resolution) =
            registered_fixture_credential(repository, FixtureCredentialAuthorityModeV1::Verified);
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) = resolution else {
            panic!("verified fixture credential must resolve");
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (arrived, request_arrived) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let mut connections = 0_usize;
            // Explicit refresh: a complete collection needs two agreeing scans.
            for _ in 0..2 {
                let (mut review, _) = listener.accept().unwrap();
                connections += 1;
                let _ = read_http_request_with_headers(&mut review);
                write_http_json(&mut review, &json!([]));
            }
            // Second explicit refresh, held at its first provider read.
            let (mut held, _) = listener.accept().unwrap();
            connections += 1;
            let _ = read_http_request_with_headers(&mut held);
            arrived.send(()).unwrap();
            released.recv().unwrap();
            drop(held);
            listener.set_nonblocking(true).unwrap();
            let unexpected = std::iter::from_fn(|| listener.accept().ok()).count();
            connections + unexpected
        });
        let client = local_client(
            address,
            GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: repository.to_owned(),
                pull_request_number: 421,
                pull_request_id: request.pull_request_id.clone(),
            },
            credential,
        );
        let store = ProjectGitHubReviewStoreV1::new(review_database, scope.clone()).unwrap();
        let decoder = GitHubOfficialResponseDecoderV1::new(
            GitHubReviewProviderIdentityV1 {
                provider: ProviderId::new("provider.github").unwrap(),
                repository_owner: "ScriptedAlchemy".to_owned(),
                repository_name: repository.to_owned(),
                pull_request_number: 421,
                base_commit_id: CommitId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
                head_commit_id: scope.head_commit_id.clone(),
                merge_base_commit_id: CommitId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                    .unwrap(),
            },
            EmptyReviewAnchors,
        )
        .unwrap();
        let transport =
            GitHubReadOnlyRuntimeTransportV1::new(store.clone(), client.clone(), decoder);
        let connector = GitHubReadOnlyConnector::new(
            GitHubReadOnlyDescriptorSetV1::new(vec![rest_descriptor(request.operation)]).unwrap(),
            transport,
            EmptyReviewAnchors,
        )
        .unwrap();
        let source_access: Arc<dyn GitHubSourceAccessAuthorityV1> = Arc::new(ReadySource);
        let anchors =
            ProjectGitHubStackAnchorAuthorityV1::new(anchor_database.clone(), scope.clone())
                .unwrap();
        let owner = Arc::new(GitHubReviewRuntimeOwnerV1 {
            coordinator: GitHubReviewRefreshCoordinatorV1::new(
                connector,
                store,
                Arc::clone(&source_access),
            ),
            source_access,
            stack_client: client,
            stack_scope: context.scope().clone(),
            stack_provider: ProviderId::new("provider.github").unwrap(),
            stack_coordinator: Arc::clone(&coordinator),
            stack_anchors: anchors,
            stack_observability: None,
        });

        assert_eq!(
            owner.retained(&context, &request).await,
            GitHubReviewRefreshOutcomeV1::Unavailable,
            "nothing is retained before an explicit refresh"
        );
        let GitHubReviewRefreshOutcomeV1::Stored(refreshed) =
            owner.refresh(&context, &request).await
        else {
            panic!("the explicit refresh must retain its agreed scans");
        };
        let GitHubReviewRefreshOutcomeV1::Stored(retained) =
            owner.retained(&context, &request).await
        else {
            panic!("local feedback must read what the refresh retained");
        };
        assert_eq!(retained.state, refreshed.state);

        // Hold the next explicit refresh inside its provider read.
        let refresh_context = context
            .clone()
            .with_cancellation(CancellationContext::active("cancel.github.held-refresh").unwrap());
        let held = {
            let (owner, request) = (Arc::clone(&owner), request.clone());
            tokio::spawn(async move { owner.refresh(&refresh_context, &request).await })
        };
        request_arrived.await.unwrap();
        let GitHubReviewRefreshOutcomeV1::Stored(during) = owner.retained(&context, &request).await
        else {
            panic!("a held refresh must not hold local retained reads");
        };
        assert_eq!(during.state, refreshed.state);
        assert!(
            !held.is_finished(),
            "the local read completed while the refresh was still held"
        );

        // Abandoning the remote refresh stops its own work only.
        held.abort();
        assert!(held.await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        let GitHubReviewRefreshOutcomeV1::Stored(after) = owner.retained(&context, &request).await
        else {
            panic!("an abandoned refresh must not disturb retained evidence");
        };
        assert_eq!(after.state, refreshed.state);

        // Local cancellation still stops local work.
        let cancelled = context.clone().with_cancellation(
            CancellationContext::cancelled("cancel.github.local", UtcMicros(5)).unwrap(),
        );
        assert_eq!(
            owner.retained(&cancelled, &request).await,
            GitHubReviewRefreshOutcomeV1::Cancelled
        );

        assert_eq!(
            server.join().unwrap(),
            3,
            "only explicit refreshes reach the provider: two agreeing scans plus the held read"
        );
    }

    #[tokio::test]
    async fn disabled_refresh_closes_provider_drift_without_a_stack_read() {
        policy_disable_journey(false).await;
    }

    #[tokio::test]
    async fn superseded_refresh_closes_provider_drift_under_current_policy() {
        policy_disable_journey(true).await;
    }

    async fn policy_disable_journey(hold_read: bool) {
        let repository = if hold_read {
            "runtime-policy-held"
        } else {
            "runtime-policy-disabled"
        };
        let mut scope = scope(repository);
        scope.head_commit_id = CommitId::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let context = context(&scope);
        let mut request = request(scope.clone());
        request.operation = GitHubReviewReadOperationV1::RestListPullRequestReviewComments;
        let storage = tempfile::tempdir().unwrap();
        let (anchor_database, _anchor_owner) = open_registered_test_database_fixture(
            &storage.path().join("project-sessions.db"),
            TestDatabaseRuntimeScope::ProjectSessions {
                project_id: scope.project_id.clone(),
            },
        )
        .await
        .unwrap();
        crate::register_test_schema_installer();
        let review_path = storage.path().join("review.db");
        let authority =
            DatabaseAuthority::acquire_test(&review_path, "github-runtime-policy").unwrap();
        let (review_database, _) = Database::publish_test_runtime(
            &review_path,
            &authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let coordinator = Arc::new(DaemonGitHubStackCoordinatorV1::default());
        coordinator
            .register_scope(
                context.scope(),
                GitHubStackedPullRequestPolicyV1::ProbePrivatePreview,
            )
            .unwrap();
        let (_credential_authority, resolution) =
            registered_fixture_credential(repository, FixtureCredentialAuthorityModeV1::Verified);
        let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) = resolution else {
            panic!("verified fixture credential must resolve");
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (arrived, request_arrived) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::channel::<()>();
        let head = scope.head_commit_id.as_str().to_owned();
        let branch = scope
            .branch_ref
            .trim_start_matches("refs/heads/")
            .to_owned();
        let server = std::thread::spawn(move || {
            let mut arrived = Some(arrived);
            for round in 0..3 {
                // Complete collection refreshes require two agreeing scans.
                for _ in 0..2 {
                    let (mut review, _) = listener.accept().unwrap();
                    let (headers, _) = read_http_request_with_headers(&mut review);
                    assert!(headers.starts_with("GET "));
                    write_http_json(&mut review, &json!([]));
                }
                if round == 2 && !hold_read {
                    continue;
                }
                let (mut stack, _) = listener.accept().unwrap();
                let (headers, body) = read_http_request_with_headers(&mut stack);
                assert!(
                    body["query"]
                        .as_str()
                        .is_some_and(|query| query.contains("stackEntry")),
                    "expected stack query, got {headers} {body}"
                );
                if round == 2 {
                    arrived.take().unwrap().send(()).unwrap();
                    released.recv().unwrap();
                }
                let base = if round == 0 {
                    "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                } else {
                    "cccccccccccccccccccccccccccccccccccccccc"
                };
                write_http_json(
                    &mut stack,
                    &json!({"data":{"repository":{"pullRequest":{
                        "stackEntry":{"position":1},
                        "stack":{"id":"runtime-stack","number":421,"baseRefName":"main","size":1,
                            "entries":{"totalCount":1,"pageInfo":{"hasNextPage":false,"endCursor":null},
                                "nodes":[{"position":1,"pullRequest":{"number":421,"baseRefName":"main",
                                    "headRefName":branch,"baseRefOid":base,"headRefOid":head,"baseRef":null,
                                    "statusCheckRollup":{"state":"SUCCESS"},"mergeQueueEntry":null}}]}}
                    }}}}),
                );
                let (mut compare, _) = listener.accept().unwrap();
                let _ = read_http_request_with_headers(&mut compare);
                write_http_json(
                    &mut compare,
                    &json!({"merge_base_commit":{"sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}),
                );
            }
        });
        let client = local_client(
            address,
            GitHubRepositoryTargetV1 {
                owner: "ScriptedAlchemy".to_owned(),
                repository: repository.to_owned(),
                pull_request_number: 421,
                pull_request_id: request.pull_request_id.clone(),
            },
            credential,
        );
        let store = ProjectGitHubReviewStoreV1::new(review_database, scope.clone()).unwrap();
        let decoder = GitHubOfficialResponseDecoderV1::new(
            GitHubReviewProviderIdentityV1 {
                provider: ProviderId::new("provider.github").unwrap(),
                repository_owner: "ScriptedAlchemy".to_owned(),
                repository_name: repository.to_owned(),
                pull_request_number: 421,
                base_commit_id: CommitId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap(),
                head_commit_id: scope.head_commit_id.clone(),
                merge_base_commit_id: CommitId::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                    .unwrap(),
            },
            EmptyReviewAnchors,
        )
        .unwrap();
        let transport =
            GitHubReadOnlyRuntimeTransportV1::new(store.clone(), client.clone(), decoder);
        let connector = GitHubReadOnlyConnector::new(
            GitHubReadOnlyDescriptorSetV1::new(vec![rest_descriptor(request.operation)]).unwrap(),
            transport,
            EmptyReviewAnchors,
        )
        .unwrap();
        let source_access: Arc<dyn GitHubSourceAccessAuthorityV1> = Arc::new(ReadySource);
        let anchors =
            ProjectGitHubStackAnchorAuthorityV1::new(anchor_database.clone(), scope).unwrap();
        let owner = Arc::new(GitHubReviewRuntimeOwnerV1 {
            coordinator: GitHubReviewRefreshCoordinatorV1::new(
                connector,
                store,
                Arc::clone(&source_access),
            ),
            source_access,
            stack_client: client,
            stack_scope: context.scope().clone(),
            stack_provider: ProviderId::new("provider.github").unwrap(),
            stack_coordinator: Arc::clone(&coordinator),
            stack_anchors: anchors.clone(),
            stack_observability: None,
        });
        for _ in 0..2 {
            let outcome = owner.refresh(&context, &request).await;
            assert!(outcome.has_usable_observation(), "{outcome:?}");
        }
        let binding = anchors
            .source_binding(
                &context,
                &request,
                &GitHubStackProviderOutcomeV1::Unavailable,
                tracedecay_contracts::now_micros(),
            )
            .unwrap();
        let open = coordinator
            .observe_policy(
                context.scope().clone(),
                owner.stack_provider.clone(),
                binding.clone(),
                tracedecay_contracts::now_micros(),
            )
            .unwrap();
        assert_eq!(open.capability.state, GitHubStackCapabilityStateV1::Enabled);
        assert!(
            open.drift_observations
                .iter()
                .any(|drift| drift.drift.state == IntervalStateV1::Open)
        );
        if hold_read {
            let (owner, context, request) = (Arc::clone(&owner), context.clone(), request.clone());
            let refresh = tokio::spawn(async move { owner.refresh(&context, &request).await });
            request_arrived.await.unwrap();
            coordinator
                .register_scope(&open.scope, GitHubStackedPullRequestPolicyV1::Disabled)
                .unwrap();
            release.send(()).unwrap();
            assert!(refresh.await.unwrap().has_usable_observation());
        } else {
            coordinator
                .register_scope(context.scope(), GitHubStackedPullRequestPolicyV1::Disabled)
                .unwrap();
            let outcome = owner.refresh(&context, &request).await;
            assert!(outcome.has_usable_observation(), "{outcome:?}");
        }
        server.join().unwrap();
        let mut records = anchor_database
            .read_connection()
            .query("SELECT anchor_json FROM retrieval_anchors", ())
            .await
            .unwrap();
        let mut published_disabled = false;
        while let Some(row) = records.next().await.unwrap() {
            let record: RetrievalAnchorRecord =
                serde_json::from_str(&row.get::<String>(0).unwrap()).unwrap();
            if matches!(record.target(), RetrievalAnchorTarget::GitTopology(target)
                if matches!(target.as_ref(), GitTopologyAnchorTargetV1::GitHubStackCapability(capability)
                    if capability.state == GitHubStackCapabilityStateV1::PrivatePreviewDisabled))
            {
                published_disabled = true;
            }
        }
        assert!(
            published_disabled,
            "disabled policy must be published to the real anchor store"
        );
        let settled = coordinator
            .observe_policy(
                context.scope().clone(),
                owner.stack_provider.clone(),
                binding,
                tracedecay_contracts::now_micros(),
            )
            .unwrap();
        assert_eq!(
            settled.capability.state,
            GitHubStackCapabilityStateV1::PrivatePreviewDisabled
        );
        assert!(
            settled.drift_observations.is_empty(),
            "the runtime refresh must already have closed provider drift"
        );
    }
}
