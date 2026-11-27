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
use crate::observability::{
    GitHubStackDriftRestoreReportV1, RegisteredObservabilityPortV1,
    restore_github_stack_drift_intervals,
};
use crate::stack_coordinator::{
    DaemonGitHubStackCoordinatorV1, GitHubStackCoordinatorErrorV1, GitHubStackObservationV1,
    GitHubStackProviderOutcomeV1, GitHubStackProviderSourceBindingV1, GitHubStackScopeV1,
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
    /// The registration project open mounted for this exact scope. The review
    /// owner joins it instead of reconstructing the scope key.
    pub stack_scope: GitHubStackScopeV1,
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
    stack_scope: GitHubStackScopeV1,
    stack_provider: tracedecay_domain::ProviderId,
    stack_coordinator: Arc<DaemonGitHubStackCoordinatorV1>,
    stack_anchors: super::ProjectGitHubStackAnchorAuthorityV1,
    stack_observability: Option<GitHubStackObservabilityV1>,
    /// Durable drift restoration runs once per mounted owner, before this
    /// owner publishes any observation that could open a duplicate interval.
    stack_drift_restore: tokio::sync::OnceCell<Option<GitHubStackDriftRestoreReportV1>>,
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
            // A restarted daemon rebuilds the scope's open drift intervals
            // from durable receipts before this owner can publish a sample.
            self.restore_stack_drift().await;
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
                .authorize_provider_read(self.stack_scope.scope())
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

    /// Restores persisted open drift intervals once per mounted owner. A
    /// missing Observatory lane or an unavailable store is named on the
    /// operator stream; the refresh still proceeds, and the next observation
    /// may then open a duplicate interval rather than fabricate closure.
    async fn restore_stack_drift(&self) {
        self.stack_drift_restore
            .get_or_init(|| async {
                let Some(observability) = self.stack_observability.as_ref() else {
                    tracing::info!(
                        event = "github_stack_drift_restore",
                        outcome = "unmounted",
                        "GitHub stack drift restoration has no observability store"
                    );
                    return None;
                };
                let port =
                    RegisteredObservabilityPortV1::new(observability.observation_db.as_ref());
                match restore_github_stack_drift_intervals(
                    &port,
                    self.stack_coordinator.as_ref(),
                    &self.stack_scope,
                    tracedecay_contracts::now_micros(),
                )
                .await
                {
                    Ok(report) => {
                        tracing::info!(
                            event = "github_stack_drift_restore",
                            outcome = if report.scope_already_live {
                                "already_live"
                            } else {
                                "restored"
                            },
                            scope_already_live = report.scope_already_live,
                            restored = report.restored,
                            closed_before_restart = report.closed_before_restart,
                            skipped_unbound = report.skipped_unbound,
                            pages_scanned = report.pages_scanned,
                            coverage = ?report.coverage,
                            "GitHub stack drift intervals were restored from durable receipts"
                        );
                        Some(report)
                    }
                    Err(error) => {
                        tracing::warn!(
                            event = "github_stack_drift_restore",
                            outcome = "unavailable",
                            error = ?error,
                            "GitHub stack drift intervals could not be restored from durable receipts"
                        );
                        None
                    }
                }
            })
            .await;
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
            self.stack_scope.scope().clone(),
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
    if config.stack_scope.scope() != &config.resolved_scope {
        return Err(GitHubReviewRuntimeOwnerBuildErrorV1::InvalidScope);
    }
    let stack_scope = config.stack_scope;
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
        stack_drift_restore: tokio::sync::OnceCell::new(),
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
    use tracedecay_contracts::{
        ObservabilityHorizonV1, ObservabilityQueryPort, ObservabilityQueryV1, ResolvedScope,
    };
    use tracedecay_domain::configuration::GitHubStackedPullRequestPolicyV1;
    use tracedecay_domain::feedback::{
        GitHubReviewCurrentBranchRemapV1, GitHubReviewImmutableAnchorV1,
    };
    use tracedecay_domain::{
        CommitId, GitHubStackCapabilityStateV1, GitTopologyAnchorTargetV1, IntervalStateV1,
        ObservabilityPayloadV1, ProviderId, RefId, RetrievalAnchorRecord, RetrievalAnchorTarget,
        StackDriftKindV1, UtcMicros, WorkStackDriftObservedV1, safe_work_topology_policy_v1,
    };
    use tracedecay_global_db::RegisteredGlobalDbOwnerV1;
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
    use super::super::{
        GitHubProviderLifecycleV1, GitHubReadOnlyCredentialAuthorityV1,
        ProjectGitHubStackAnchorAuthorityV1,
    };
    use super::*;
    use crate::observability::{
        BoundedObservabilityProducerV1, ObservabilityProducerIdentityV1,
        RegisteredObservabilityPortV1,
    };
    use crate::stack_coordinator::github_stack_circuit_policy;

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
        let scope_registration_1 = coordinator
            .register_scope(
                context.scope(),
                GitHubStackedPullRequestPolicyV1::Disabled,
                github_stack_circuit_policy().unwrap(),
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
            stack_scope: scope_registration_1,
            stack_provider: ProviderId::new("provider.github").unwrap(),
            stack_coordinator: Arc::clone(&coordinator),
            stack_anchors: anchors,
            stack_observability: None,
            stack_drift_restore: tokio::sync::OnceCell::new(),
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
        let scope_registration_2 = coordinator
            .register_scope(
                context.scope(),
                GitHubStackedPullRequestPolicyV1::ProbePrivatePreview,
                github_stack_circuit_policy().unwrap(),
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
            stack_scope: scope_registration_2,
            stack_provider: ProviderId::new("provider.github").unwrap(),
            stack_coordinator: Arc::clone(&coordinator),
            stack_anchors: anchors.clone(),
            stack_observability: None,
            stack_drift_restore: tokio::sync::OnceCell::new(),
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
            let _scope_registration_3 = coordinator
                .register_scope(
                    &open.scope,
                    GitHubStackedPullRequestPolicyV1::Disabled,
                    github_stack_circuit_policy().unwrap(),
                )
                .unwrap();
            release.send(()).unwrap();
            assert!(refresh.await.unwrap().has_usable_observation());
        } else {
            let _scope_registration_4 = coordinator
                .register_scope(
                    context.scope(),
                    GitHubStackedPullRequestPolicyV1::Disabled,
                    github_stack_circuit_policy().unwrap(),
                )
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

    /// Everything one exact scope needs to mount real review runtime owners
    /// the way the composition root does: the project database the anchor
    /// authority and Observatory lane bind, the review store, the shared
    /// daemon coordinator, and a verified loopback credential.
    struct RuntimeJourney {
        coordinator: Arc<DaemonGitHubStackCoordinatorV1>,
        producer: std::sync::Mutex<Arc<BoundedObservabilityProducerV1>>,
        project_database: RegisteredGlobalDbLeaseV1,
        review_database: Database,
        scope: FeedbackScopeV1,
        context: RequestContext,
        request: GitHubReviewReadRequestV1,
        credential: GitHubReadOnlyCredentialV1,
        _storage: tempfile::TempDir,
        _project_owner: RegisteredGlobalDbOwnerV1,
        _credential_authority: Arc<dyn GitHubReadOnlyCredentialAuthorityV1>,
    }

    const RUNTIME_HEAD: &str = "dddddddddddddddddddddddddddddddddddddddd";
    const MERGE_BASE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ADVANCED_BASE: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const FURTHER_ADVANCED_BASE: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    const DRIFT_EVENT_KIND: &str = "work.stack_drift.observed.v1";

    /// One stack GraphQL response whose single entry is the selected pull
    /// request, so the read binds an exact provider snapshot.
    fn stack_response(base: &str, branch: &str) -> serde_json::Value {
        json!({"data":{"repository":{"pullRequest":{
            "stackEntry":{"position":1},
            "stack":{"id":"runtime-stack","number":421,"baseRefName":"main","size":1,
                "entries":{"totalCount":1,"pageInfo":{"hasNextPage":false,"endCursor":null},
                    "nodes":[{"position":1,"pullRequest":{"number":421,"baseRefName":"main",
                        "headRefName":branch,"baseRefOid":base,"headRefOid":RUNTIME_HEAD,"baseRef":null,
                        "statusCheckRollup":{"state":"SUCCESS"},"mergeQueueEntry":null}}]}}
        }}}})
    }

    /// Serves the two agreeing review scans one complete refresh needs.
    fn serve_review_scans(listener: &TcpListener) {
        for _ in 0..2 {
            let (mut review, _) = listener.accept().unwrap();
            let _ = read_http_request_with_headers(&mut review);
            write_http_json(&mut review, &json!([]));
        }
    }

    /// Serves the compare read that binds each layer's merge base.
    fn serve_compare(listener: &TcpListener) {
        let (mut compare, _) = listener.accept().unwrap();
        let _ = read_http_request_with_headers(&mut compare);
        write_http_json(
            &mut compare,
            &json!({"merge_base_commit":{"sha":MERGE_BASE}}),
        );
    }

    /// Serves one complete refresh: two agreeing review scans, one stack
    /// GraphQL read and one compare read.
    fn serve_refresh(listener: &TcpListener, base: &str, branch: &str) {
        serve_review_scans(listener);
        let (mut stack, _) = listener.accept().unwrap();
        let (headers, body) = read_http_request_with_headers(&mut stack);
        assert!(
            body["query"]
                .as_str()
                .is_some_and(|query| query.contains("stackEntry")),
            "expected stack query, got {headers} {body}"
        );
        write_http_json(&mut stack, &stack_response(base, branch));
        serve_compare(listener);
    }

    fn start_producer(
        project_database: &RegisteredGlobalDbLeaseV1,
        scope: &FeedbackScopeV1,
    ) -> Arc<BoundedObservabilityProducerV1> {
        Arc::new(
            BoundedObservabilityProducerV1::start(
                project_database.clone(),
                ObservabilityProducerIdentityV1 {
                    authorized_scope_ref: scope.project_id.as_str().to_owned(),
                    process_boot_id: "boot:github-runtime-lifetime".to_owned(),
                    producer_revision: "github-runtime-lifetime.v1".to_owned(),
                    configuration_revision: "github-runtime-lifetime.v1".to_owned(),
                    policy_revision: "github-runtime-lifetime.v1".to_owned(),
                },
                256,
            )
            .unwrap(),
        )
    }

    impl RuntimeJourney {
        async fn mount(repository: &str) -> Self {
            let mut scope = scope(repository);
            scope.head_commit_id = CommitId::new(RUNTIME_HEAD).unwrap();
            let context = context(&scope);
            let mut request = request(scope.clone());
            request.operation = GitHubReviewReadOperationV1::RestListPullRequestReviewComments;
            let storage = tempfile::tempdir().unwrap();
            let (project_database, project_owner) = open_registered_test_database_fixture(
                &storage.path().join("project-sessions.db"),
                TestDatabaseRuntimeScope::ProjectSessions {
                    project_id: scope.project_id.clone(),
                },
            )
            .await
            .unwrap();
            let producer = start_producer(&project_database, &scope);
            crate::register_test_schema_installer();
            let review_path = storage.path().join("review.db");
            let authority =
                DatabaseAuthority::acquire_test(&review_path, "github-runtime-lifetime").unwrap();
            let (review_database, _) = Database::publish_test_runtime(
                &review_path,
                &authority,
                TestDatabaseRuntimeMode::Initialize,
            )
            .await
            .unwrap();
            let (credential_authority, resolution) = registered_fixture_credential(
                repository,
                FixtureCredentialAuthorityModeV1::Verified,
            );
            let RegisteredGitHubReadOnlyCredentialV1::Verified(credential) = resolution else {
                panic!("verified fixture credential must resolve");
            };
            Self {
                coordinator: Arc::new(DaemonGitHubStackCoordinatorV1::default()),
                producer: std::sync::Mutex::new(producer),
                project_database,
                review_database,
                scope,
                context,
                request,
                credential,
                _storage: storage,
                _project_owner: project_owner,
                _credential_authority: credential_authority,
            }
        }

        /// Mounts one review runtime owner for `scope` on `registration`, beside
        /// the Observatory lane the composition root mounts for it. The
        /// production builder admits only `https` providers, so the loopback
        /// fixture mounts the same owner shape directly.
        fn owner(
            &self,
            scope: &FeedbackScopeV1,
            registration: &GitHubStackScopeV1,
            address: std::net::SocketAddr,
        ) -> GitHubReviewRuntimeOwnerV1<EmptyReviewAnchors, EmptyReviewAnchors> {
            let repository_name = scope
                .repository_id
                .as_str()
                .trim_start_matches("repository.github.")
                .to_owned();
            let client = local_client(
                address,
                GitHubRepositoryTargetV1 {
                    owner: "ScriptedAlchemy".to_owned(),
                    repository: repository_name.clone(),
                    pull_request_number: 421,
                    pull_request_id: self.request.pull_request_id.clone(),
                },
                self.credential.clone(),
            );
            let store =
                ProjectGitHubReviewStoreV1::new(self.review_database.clone(), scope.clone())
                    .unwrap();
            let decoder = GitHubOfficialResponseDecoderV1::new(
                GitHubReviewProviderIdentityV1 {
                    provider: ProviderId::new("provider.github").unwrap(),
                    repository_owner: "ScriptedAlchemy".to_owned(),
                    repository_name,
                    pull_request_number: 421,
                    base_commit_id: CommitId::new(MERGE_BASE).unwrap(),
                    head_commit_id: scope.head_commit_id.clone(),
                    merge_base_commit_id: CommitId::new(MERGE_BASE).unwrap(),
                },
                EmptyReviewAnchors,
            )
            .unwrap();
            let transport =
                GitHubReadOnlyRuntimeTransportV1::new(store.clone(), client.clone(), decoder);
            let connector = GitHubReadOnlyConnector::new(
                GitHubReadOnlyDescriptorSetV1::new(vec![rest_descriptor(self.request.operation)])
                    .unwrap(),
                transport,
                EmptyReviewAnchors,
            )
            .unwrap();
            let source_access: Arc<dyn GitHubSourceAccessAuthorityV1> = Arc::new(ReadySource);
            GitHubReviewRuntimeOwnerV1 {
                coordinator: GitHubReviewRefreshCoordinatorV1::new(
                    connector,
                    store,
                    Arc::clone(&source_access),
                ),
                source_access,
                stack_client: client,
                stack_scope: registration.clone(),
                stack_provider: ProviderId::new("provider.github").unwrap(),
                stack_coordinator: Arc::clone(&self.coordinator),
                stack_anchors: ProjectGitHubStackAnchorAuthorityV1::new(
                    self.project_database.clone(),
                    scope.clone(),
                )
                .unwrap(),
                stack_observability: Some(GitHubStackObservabilityV1 {
                    probe_owner: GitHubStackProbeOwnerV1::mount(
                        registration.scope().clone(),
                        safe_work_topology_policy_v1(),
                        "ScriptedAlchemy",
                        "tracedecay",
                        true,
                    )
                    .unwrap(),
                    producer: Arc::clone(&self.producer.lock().unwrap()),
                    observation_db: self.project_database.clone(),
                }),
                stack_drift_restore: tokio::sync::OnceCell::new(),
            }
        }

        /// The daemon restart boundary: the current producer drains and joins
        /// so every receipt it accepted is durable, and a fresh producer
        /// serves the next mount exactly as a restarted daemon would.
        async fn restart_observability(&self) {
            self.producer
                .lock()
                .unwrap()
                .shutdown()
                .await
                .expect("flush the mounted producer");
            *self.producer.lock().unwrap() = start_producer(&self.project_database, &self.scope);
        }

        /// Capability and snapshot anchors the mounted refreshes published to
        /// the retrieval-anchor authority.
        async fn stack_anchors(&self) -> Vec<GitTopologyAnchorTargetV1> {
            let mut records = self
                .project_database
                .read_connection()
                .query("SELECT anchor_json FROM retrieval_anchors", ())
                .await
                .unwrap();
            let mut targets = Vec::new();
            while let Some(row) = records.next().await.unwrap() {
                let record: RetrievalAnchorRecord =
                    serde_json::from_str(&row.get::<String>(0).unwrap()).unwrap();
                if let RetrievalAnchorTarget::GitTopology(target) = record.target() {
                    targets.push(target.as_ref().clone());
                }
            }
            targets
        }

        /// Durable drift receipts the mounted refreshes published, read after
        /// the producer has flushed them.
        async fn drift_receipts(&self) -> Vec<WorkStackDriftObservedV1> {
            self.restart_observability().await;
            RegisteredObservabilityPortV1::new(self.project_database.as_ref())
                .query(ObservabilityQueryV1 {
                    authorized_scope_ref: self.scope.project_id.as_str().to_owned(),
                    event_kinds: vec![DRIFT_EVENT_KIND.to_owned()],
                    horizon: ObservabilityHorizonV1 {
                        since_micros: 0,
                        until_micros: i64::MAX,
                    },
                    after_watermark: None,
                    limit: 256,
                })
                .await
                .unwrap()
                .events
                .into_iter()
                .map(|envelope| match envelope.payload {
                    ObservabilityPayloadV1::WorkStackDrift(drift) => drift,
                    other => panic!("unexpected payload {other:?}"),
                })
                .collect()
        }
    }

    /// A stack read that outlives the registration that authorized it cannot
    /// publish into the replacement registration of the same logical scope: it
    /// emits no drift receipt for the base advance it observed.
    #[tokio::test]
    async fn late_read_cannot_publish_into_a_replacement_registration() {
        let journey = RuntimeJourney::mount("runtime-late-read").await;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let circuit = github_stack_circuit_policy().unwrap();
        let registration = journey
            .coordinator
            .register_scope(
                journey.context.scope(),
                GitHubStackedPullRequestPolicyV1::ProbePrivatePreview,
                circuit.clone(),
            )
            .unwrap();
        let owner = Arc::new(journey.owner(&journey.scope, &registration, address));
        let (arrived, request_arrived) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::channel::<()>();
        let branch = journey
            .scope
            .branch_ref
            .trim_start_matches("refs/heads/")
            .to_owned();
        let server = std::thread::spawn(move || {
            serve_review_scans(&listener);
            let (mut stack, _) = listener.accept().unwrap();
            let _ = read_http_request_with_headers(&mut stack);
            arrived.send(()).unwrap();
            released.recv().unwrap();
            write_http_json(&mut stack, &stack_response(ADVANCED_BASE, &branch));
            serve_compare(&listener);
        });

        // The read is authorized by the probe registration, then held.
        let (held_owner, context, request) = (
            Arc::clone(&owner),
            journey.context.clone(),
            journey.request.clone(),
        );
        let held = tokio::spawn(async move { held_owner.refresh(&context, &request).await });
        request_arrived.await.unwrap();

        // The scope's policy changes while the read is in flight, which
        // replaces the registration the read was authorized under.
        let replacement = journey
            .coordinator
            .register_scope(
                journey.context.scope(),
                GitHubStackedPullRequestPolicyV1::Disabled,
                circuit,
            )
            .unwrap();
        release.send(()).unwrap();
        let held = held.await.unwrap();
        assert!(
            held.has_usable_observation(),
            "the review refresh itself still completes: {held:?}"
        );
        server.join().unwrap();

        // The late read published nothing: it opened no drift receipt and
        // wrote no provider snapshot anchor for the stack it observed.
        let receipts = journey.drift_receipts().await;
        assert!(
            receipts.is_empty(),
            "a superseded read must not publish drift: {receipts:?}"
        );
        let anchors = journey.stack_anchors().await;
        assert!(
            !anchors
                .iter()
                .any(|target| matches!(target, GitTopologyAnchorTargetV1::GitHubStackSnapshot(_))),
            "a superseded read must not publish a provider snapshot: {anchors:?}"
        );
        drop(owner);
        drop(registration);
        drop(replacement);
        assert_eq!(journey.coordinator.retained_scope_count(), 0);
    }

    /// Retiring a mounted owner releases its scope state. After a restart the
    /// fresh mount restores the one open drift interval from durable receipts
    /// and closes it exactly once, on that interval's original trace, once the
    /// provider no longer reports the change.
    #[tokio::test]
    async fn retired_owner_releases_state_and_a_fresh_mount_closes_its_drift_once() {
        let journey = RuntimeJourney::mount("runtime-remount").await;
        let probe = GitHubStackedPullRequestPolicyV1::ProbePrivatePreview;
        let circuit = github_stack_circuit_policy().unwrap();
        let branch = journey
            .scope
            .branch_ref
            .trim_start_matches("refs/heads/")
            .to_owned();

        // The first mount observes the stack, then observes its base advance,
        // which opens one drift interval. A third refresh sees the base
        // advance again and must keep that one interval on its original trace.
        let first = journey
            .coordinator
            .register_scope(journey.context.scope(), probe, circuit.clone())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let first_branch = branch.clone();
        let server = std::thread::spawn(move || {
            serve_refresh(&listener, MERGE_BASE, &first_branch);
            serve_refresh(&listener, ADVANCED_BASE, &first_branch);
            serve_refresh(&listener, FURTHER_ADVANCED_BASE, &first_branch);
        });
        let owner = journey.owner(&journey.scope, &first, address);
        for _ in 0..3 {
            let outcome = owner.refresh(&journey.context, &journey.request).await;
            assert!(outcome.has_usable_observation(), "{outcome:?}");
        }
        server.join().unwrap();
        assert_eq!(journey.coordinator.retained_scope_count(), 1);
        drop(owner);
        drop(first);
        assert_eq!(
            journey.coordinator.retained_scope_count(),
            0,
            "retiring the mounted owner releases its scope state"
        );

        // The daemon restarts: the retired owner's receipts become durable
        // before a fresh mount of the same scope reads them back.
        journey.restart_observability().await;
        let second = journey
            .coordinator
            .register_scope(journey.context.scope(), probe, circuit)
            .unwrap();
        assert_eq!(journey.coordinator.retained_scope_count(), 1);
        // The provider reports the same further-advanced base, so this
        // observation adds no new drift and the restored interval closes.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let remount_branch = branch.clone();
        let server = std::thread::spawn(move || {
            serve_refresh(&listener, FURTHER_ADVANCED_BASE, &remount_branch)
        });
        let owner = journey.owner(&journey.scope, &second, address);
        let outcome = owner.refresh(&journey.context, &journey.request).await;
        assert!(outcome.has_usable_observation(), "{outcome:?}");
        server.join().unwrap();

        let receipts = journey.drift_receipts().await;
        let base_advanced: Vec<_> = receipts
            .iter()
            .filter(|drift| drift.kind == StackDriftKindV1::BaseAdvanced)
            .collect();
        // A base that keeps advancing stays one interval: every receipt it
        // produces, open or closed, carries the trace that opened it.
        assert_eq!(
            base_advanced.len(),
            3,
            "two observations of the advance and its close: {receipts:?}"
        );
        let first_observed = base_advanced[0].first_observed_micros;
        assert!(
            base_advanced
                .iter()
                .all(|drift| drift.first_observed_micros == first_observed),
            "a persisting drift keeps its original trace instead of opening a new interval: {receipts:?}"
        );
        assert_eq!(base_advanced[0].state, IntervalStateV1::Open);
        assert_eq!(base_advanced[1].state, IntervalStateV1::Open);
        assert_eq!(base_advanced[2].state, IntervalStateV1::Closed);
        drop(owner);
        drop(second);
        assert_eq!(journey.coordinator.retained_scope_count(), 0);
    }

    /// A retired mount whose cleanup runs after its replacement registered
    /// cannot remove that replacement. The second holder keeps the old mount
    /// alive across the replacement's registration, then releases it.
    #[tokio::test]
    async fn late_cleanup_of_a_retired_mount_cannot_remove_its_replacement() {
        let journey = RuntimeJourney::mount("runtime-late-cleanup").await;
        let probe = GitHubStackedPullRequestPolicyV1::ProbePrivatePreview;
        let circuit = github_stack_circuit_policy().unwrap();
        let retired = journey
            .coordinator
            .register_scope(journey.context.scope(), probe, circuit.clone())
            .unwrap();
        let late_holder = retired.clone();
        drop(retired);
        assert_eq!(
            journey.coordinator.retained_scope_count(),
            1,
            "a second holder keeps the retired mount mounted"
        );

        let replacement = journey
            .coordinator
            .register_scope(journey.context.scope(), probe, circuit)
            .unwrap();
        // The retired mount's cleanup runs now, after its replacement exists.
        drop(late_holder);
        assert_eq!(
            journey.coordinator.retained_scope_count(),
            1,
            "a retired mount's late cleanup must not remove its replacement"
        );
        assert!(
            journey
                .coordinator
                .authorize_provider_read(journey.context.scope())
                .unwrap()
                .is_some(),
            "the replacement still authorizes provider reads after the old mount is cleaned up"
        );
        drop(replacement);
        assert_eq!(
            journey.coordinator.retained_scope_count(),
            0,
            "no scope state outlives the last mounted owner"
        );
    }

    /// Many mounted runtimes each retain exactly their own scope, and dropping
    /// them releases every scope.
    #[tokio::test]
    async fn mounted_runtimes_release_every_scope_on_drop() {
        let journey = RuntimeJourney::mount("runtime-many").await;
        let probe = GitHubStackedPullRequestPolicyV1::ProbePrivatePreview;
        let circuit = github_stack_circuit_policy().unwrap();
        let address = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let mut owners = Vec::new();
        for index in 0..24 {
            let mut scope = journey.scope.clone();
            scope.worktree_id =
                tracedecay_domain::WorktreeId::new(format!("worktree.github.runtime-many.{index}"))
                    .unwrap();
            let resolved = ResolvedScope::new(
                scope.project_id.clone(),
                scope.repository_id.clone(),
                scope.worktree_id.clone(),
                Some(RefId::new(scope.branch_ref.clone()).unwrap()),
            )
            .unwrap();
            let registration = journey
                .coordinator
                .register_scope(&resolved, probe, circuit.clone())
                .unwrap();
            owners.push((journey.owner(&scope, &registration, address), registration));
            assert_eq!(
                journey.coordinator.retained_scope_count(),
                index + 1,
                "each mounted runtime retains exactly its own scope"
            );
        }
        drop(owners);
        assert_eq!(
            journey.coordinator.retained_scope_count(),
            0,
            "no scope state outlives the last mounted owner"
        );
    }
}
