//! Canonical external-source acquisition over the existing GitHub review owner.

use std::collections::BTreeSet;
use std::sync::Arc;

use tracedecay_application::feedback::{
    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GitHubReviewReadRequestV1, feedback_surface_operation,
};
use tracedecay_application::{AuthorizationPhase, AuthorizationRequest, RequestContext};
use tracedecay_domain::configuration::SourceKindV1;
use tracedecay_domain::feedback::{GitHubReviewIngressProviderOutcomeV1, GitHubReviewLifecycleV1};
use tracedecay_domain::{
    ManifestDigest, ObservationScopeV1, ProviderId, SourceAcquisitionCapabilitiesV1,
    SourceAcquisitionContractV1, SourceBindingOwnerV1, SourceBindingV1, SourceCaptureModeV1,
    SourceContentStateV1, SourceCoverageV1, SourceDefinitionV1, SourceDeletionSemanticsV1,
    SourceEnvelopeKindV1, SourceEventV1, SourceInstanceId, SourceNativeObjectIdV1,
    SourceObjectObservationV1, SourceObjectRevisionV1, SourcePartitionIdV1,
    SourceProviderEnvelopeV1, SourceRefetchStrategyV1, SourceSnapshotIdV1, canonical_sha256,
};
use tracedecay_store::{
    SourceObjectMutationV1, SourceObjectTransitionV1, SourceObservationEvidenceV1,
    build_scope_resolution_authorization_v1,
};

use crate::advisory::{
    GitHubCanonicalReviewAnchorAuthorityV1, GitHubCurrentBranchRemapper, GitHubProviderLifecycleV1,
    GitHubReviewBodyEvidenceAuthorityV1, GitHubReviewBodyReadOutcomeV1,
    GitHubReviewRefreshOutcomeV1, GitHubReviewRuntimeOwnerV1,
};
use crate::configuration::ConfigurationControlStore;
use crate::external_source_acquisition::{
    SourceAcquisitionAuthorizationOutcomeV1, SourceAcquisitionAuthorizationPhaseV1,
    SourceAcquisitionAuthorizationPortV1, SourceAcquisitionFuture, SourceAcquisitionGrantV1,
    SourceAcquisitionRequestV1, SourceCanonicalRefetchOutcomeV1, SourceCanonicalRefetchPageV1,
    SourceCanonicalRefetchPortV1, SourceScheduledRefetchV1,
};
use crate::external_source_store::RuntimeExternalSourceStore;
use crate::observation::ObservationCancellation;
use crate::source_authorization::{
    ProjectSourceAccessOutcome, ProjectSourceAccessSnapshot,
    project_source_access_snapshot_for_request,
};

const GITHUB_EXTERNAL_SOURCE_AUTHORITY_V1: &str = "tracedecay.github-review.external-source.v1";
const GITHUB_EXTERNAL_SOURCE_ID_V1: &str = "source.github-review.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubExternalSourceOpenErrorV1 {
    InvalidAuthority,
    InvalidSource,
}

pub struct GitHubExternalSourceAcquisitionV1<R, A, C> {
    github: Arc<GitHubReviewRuntimeOwnerV1<R, A>>,
    store: RuntimeExternalSourceStore,
    configuration: C,
    context: RequestContext,
    acquisition_request: SourceAcquisitionRequestV1,
    definition: SourceDefinitionV1,
    binding: SourceBindingV1,
    grant: SourceAcquisitionGrantV1,
    source_access: ProjectSourceAccessSnapshot,
    request_grant_digest: ManifestDigest,
}

impl<R, A, C> GitHubExternalSourceAcquisitionV1<R, A, C>
where
    R: GitHubCurrentBranchRemapper + Send + Sync,
    A: GitHubCanonicalReviewAnchorAuthorityV1
        + GitHubReviewBodyEvidenceAuthorityV1
        + Clone
        + Send
        + Sync,
    C: ConfigurationControlStore + Clone + Send + Sync,
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        github: Arc<GitHubReviewRuntimeOwnerV1<R, A>>,
        store: RuntimeExternalSourceStore,
        configuration: C,
        context: RequestContext,
        request: GitHubReviewReadRequestV1,
        source_access: &ProjectSourceAccessSnapshot,
        provider: ProviderId,
        sink_revision: u64,
        sink_digest: ManifestDigest,
    ) -> Result<Self, GitHubExternalSourceOpenErrorV1> {
        context
            .validate()
            .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidAuthority)?;
        request
            .validate()
            .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?;
        if source_access.scope != *context.scope()
            || source_access.requester != *context.actor()
            || source_access.binding.source_kind != SourceKindV1::GitHub
            || source_access.binding.authority
                != tracedecay_domain::configuration::AuthorityRef::Project(
                    context.scope().project_id.clone(),
                )
        {
            return Err(GitHubExternalSourceOpenErrorV1::InvalidAuthority);
        }
        let capabilities = SourceAcquisitionCapabilitiesV1::new(
            [SourceCaptureModeV1::Event].into_iter().collect(),
            [SourceRefetchStrategyV1::WholeRoot].into_iter().collect(),
            [SourceDeletionSemanticsV1::CompleteSnapshotAbsence]
                .into_iter()
                .collect(),
        )
        .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?;
        let definition = SourceDefinitionV1::new(
            SourceInstanceId::new(GITHUB_EXTERNAL_SOURCE_ID_V1)
                .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?,
            1,
            SourceAcquisitionContractV1::new(provider, capabilities)
                .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?,
            SourceCaptureModeV1::Event,
            SourceRefetchStrategyV1::WholeRoot,
            SourceDeletionSemanticsV1::CompleteSnapshotAbsence,
            1,
        )
        .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?;
        let acquisition_request = SourceAcquisitionRequestV1::github_review(
            definition.provider.clone(),
            source_access.binding.source_locator_digest.clone(),
            request.scope.clone(),
            request.operation,
            request.pull_request_id.clone(),
        )
        .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?;
        let source_anchor = tracedecay_domain::RetrievalAnchorId::new(format!(
            "anchor.github-source.{}",
            digest_suffix(
                &canonical_sha256(&(
                    GITHUB_EXTERNAL_SOURCE_AUTHORITY_V1,
                    &request.scope,
                    &request.pull_request_id,
                ))
                .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?
            )
            .ok_or(GitHubExternalSourceOpenErrorV1::InvalidSource)?
        ))
        .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?;
        let authorization = build_scope_resolution_authorization_v1(
            &ObservationScopeV1::Project {
                project_id: context.scope().project_id.clone(),
            },
            &source_anchor,
            GITHUB_EXTERNAL_SOURCE_AUTHORITY_V1,
        )
        .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidAuthority)?;
        let binding = SourceBindingV1::new(
            &definition,
            SourceBindingOwnerV1::Project(context.scope().project_id.clone()),
            authorization.privacy_domain_id,
            acquisition_request
                .binding_native_root()
                .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?,
            1,
        )
        .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?;
        let source_authorization_digest = canonical_sha256(&(
            "tracedecay.github-review.external-source-authorization.v1",
            &definition,
            &binding,
            &source_access.configuration_revision,
            &source_access.configuration_digest,
            &source_access.configuration_provenance_digest,
            context.grant().revision,
            &context.grant().digest,
            sink_revision,
            &sink_digest,
        ))
        .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidAuthority)?;
        let grant = SourceAcquisitionGrantV1::new(
            context.grant().revision,
            source_access.configuration_digest.clone(),
            sink_revision,
            sink_digest,
            source_authorization_digest,
        )
        .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidAuthority)?;
        Ok(Self {
            github,
            store,
            configuration,
            request_grant_digest: context.grant().digest.clone(),
            context,
            acquisition_request,
            definition,
            binding,
            grant,
            source_access: source_access.clone(),
        })
    }

    pub fn definition(&self) -> &SourceDefinitionV1 {
        &self.definition
    }

    pub fn binding(&self) -> &SourceBindingV1 {
        &self.binding
    }

    pub fn acquisition_request(&self) -> &SourceAcquisitionRequestV1 {
        &self.acquisition_request
    }

    pub fn event(
        &self,
        stable_signal_digest: ManifestDigest,
    ) -> Result<SourceEventV1, GitHubExternalSourceOpenErrorV1> {
        SourceEventV1::new(
            self.binding
                .immutable_identity()
                .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?,
            canonical_sha256(&(
                "tracedecay.github-review.external-source-event-signal.v1",
                self.acquisition_request.request_digest(),
                stable_signal_digest,
            ))
            .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)?,
        )
        .map_err(|_| GitHubExternalSourceOpenErrorV1::InvalidSource)
    }

    async fn current_grant(
        &self,
        task: &SourceScheduledRefetchV1,
    ) -> SourceAcquisitionAuthorizationOutcomeV1 {
        let Some(request) = github_review_request(task.request()) else {
            return SourceAcquisitionAuthorizationOutcomeV1::Unauthorized;
        };
        match self.github.authorize(&self.context, &request).await {
            GitHubProviderLifecycleV1::Ready => {}
            GitHubProviderLifecycleV1::Unavailable => {
                return SourceAcquisitionAuthorizationOutcomeV1::Unavailable;
            }
            GitHubProviderLifecycleV1::Denied
            | GitHubProviderLifecycleV1::Stale
            | GitHubProviderLifecycleV1::Ambiguous => {
                return SourceAcquisitionAuthorizationOutcomeV1::Unauthorized;
            }
        }
        let operation = feedback_surface_operation("github_review_ingest")
            .ok()
            .flatten();
        let Some(operation) = operation else {
            return SourceAcquisitionAuthorizationOutcomeV1::Unavailable;
        };
        if operation.capability_id().as_str() != GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1 {
            return SourceAcquisitionAuthorizationOutcomeV1::Unavailable;
        }
        let observed_at = tracedecay_application::now_micros();
        let request = AuthorizationRequest {
            context: &self.context,
            operation: &operation,
            phase: AuthorizationPhase::Admission,
            observed_at,
        };
        let ProjectSourceAccessOutcome::Allowed(snapshot) =
            project_source_access_snapshot_for_request(
                &self.configuration,
                &request,
                SourceKindV1::GitHub,
            )
            .await
        else {
            return SourceAcquisitionAuthorizationOutcomeV1::Unauthorized;
        };
        if &snapshot.scope != &self.source_access.scope
            || &snapshot.requester != &self.source_access.requester
            || &snapshot.binding != &self.source_access.binding
            || &snapshot.configuration_revision != &self.source_access.configuration_revision
            || &snapshot.configuration_digest != &self.source_access.configuration_digest
            || &snapshot.configuration_provenance_digest
                != &self.source_access.configuration_provenance_digest
            || &snapshot.effective_capabilities != &self.source_access.effective_capabilities
            || self.context.grant().revision != self.grant.configuration_revision
            || &self.context.grant().digest != &self.request_grant_digest
        {
            return SourceAcquisitionAuthorizationOutcomeV1::Unauthorized;
        }
        SourceAcquisitionAuthorizationOutcomeV1::Authorized(self.grant.clone())
    }

    async fn refetch_page(
        &self,
        task: &SourceScheduledRefetchV1,
        grant: &SourceAcquisitionGrantV1,
        cancellation: &ObservationCancellation,
    ) -> Option<SourceCanonicalRefetchPageV1> {
        if cancellation.is_cancelled()
            || task.definition() != &self.definition
            || task.binding() != &self.binding
            || task.request() != &self.acquisition_request
            || grant != &self.grant
        {
            return None;
        }
        let request = github_review_request(task.request())?;
        let GitHubReviewRefreshOutcomeV1::Stored(receipt) =
            self.github.refresh(&self.context, &request).await
        else {
            return None;
        };
        let response = &receipt.state.latest_attempt;
        if response.validate_for(&request).is_err()
            || response.ingress.outcome != GitHubReviewIngressProviderOutcomeV1::Complete
            || response.ingress.items.len() > tracedecay_store::MAX_SOURCE_COMMIT_OBSERVATIONS_V1
        {
            return None;
        }
        let binding = self.binding.immutable_identity().ok()?;
        let partition = SourcePartitionIdV1::new(
            canonical_sha256(&(
                "tracedecay.github-review.external-source-partition.v1",
                &request.scope.repository_id,
                &request.pull_request_id,
                request.operation,
            ))
            .ok()?,
        );
        let current = self.store.read_state(binding.clone()).await.ok()?;
        let mut mutations = Vec::new();
        let mut present_objects = BTreeSet::new();
        for item in &response.ingress.items {
            let native_object = SourceNativeObjectIdV1::new(
                canonical_sha256(&(
                    "tracedecay.github-review.external-source-object.v1",
                    &item.repository_id,
                    &item.pull_request_id,
                    &item.comment_id,
                ))
                .ok()?,
            );
            if item.lifecycle == GitHubReviewLifecycleV1::Deleted {
                continue;
            }
            present_objects.insert(native_object.clone());
            let GitHubReviewBodyReadOutcomeV1::Current(body) = self
                .github
                .expand_retained_body(&self.context, &request, &item.body_anchor)
                .await
            else {
                return None;
            };
            if body.body_anchor != item.body_anchor || body.provider_body_digest != item.body_digest
            {
                return None;
            }
            let observation = SourceObjectObservationV1::new(
                native_object.clone(),
                SourceObjectRevisionV1::new(item.version_digest.clone()),
                body.retained_body_digest.clone(),
                SourceContentStateV1::Live,
            )
            .ok()?;
            let prior = current
                .as_ref()
                .and_then(|state| state.projected_objects().get(&native_object));
            if prior == Some(&observation) {
                continue;
            }
            if prior.is_some_and(|prior| prior.revision() == observation.revision()) {
                return None;
            }
            let authorization = build_scope_resolution_authorization_v1(
                &ObservationScopeV1::Project {
                    project_id: self.context.scope().project_id.clone(),
                },
                &body.body_anchor,
                GITHUB_EXTERNAL_SOURCE_AUTHORITY_V1,
            )
            .ok()?;
            let evidence = SourceObservationEvidenceV1::new(
                binding.clone(),
                partition.clone(),
                &observation,
                body.sanitization_receipt.receipt().clone(),
                body.body_anchor.clone(),
                authorization,
                grant.source_authorization_digest.clone(),
            )
            .ok()?;
            let transition = match prior {
                None => SourceObjectTransitionV1::Initial,
                Some(prior)
                    if prior.content_state() == SourceContentStateV1::AuthoritativeDeleted =>
                {
                    SourceObjectTransitionV1::Reappearance
                }
                Some(_) if item.lifecycle == GitHubReviewLifecycleV1::Edited => {
                    SourceObjectTransitionV1::Correction
                }
                Some(_) => SourceObjectTransitionV1::Successor,
            };
            mutations.push(
                SourceObjectMutationV1::new(
                    observation,
                    prior.map(|prior| prior.revision().clone()),
                    transition,
                    evidence,
                )
                .ok()?,
            );
        }
        let snapshot = SourceSnapshotIdV1::new(
            canonical_sha256(&(
                "tracedecay.github-review.external-source-snapshot.v1",
                task.refresh().refresh_id(),
                &response.ingress.provider_base_commit_id,
                &response.ingress.provider_head_commit_id,
                &response.ingress.merge_base_commit_id,
                request.operation,
            ))
            .ok()?,
        );
        let sanitized_envelope_digest = canonical_sha256(&(
            "tracedecay.github-review.external-source-envelope.v1",
            &response.ingress,
        ))
        .ok()?;
        let envelope = SourceProviderEnvelopeV1::new(
            binding,
            self.definition.provider.clone(),
            task.refresh().refresh_id().clone(),
            tracedecay_domain::SourceRefreshCauseV1::Event,
            self.definition.capture_mode,
            self.definition.refetch_strategy,
            SourceEnvelopeKindV1::WholeRoot,
            partition,
            1,
            None,
            None,
            Some(snapshot),
            SourceCoverageV1::Complete,
            sanitized_envelope_digest,
        )
        .ok()?;
        Some(SourceCanonicalRefetchPageV1 {
            envelope,
            mutations,
            present_objects,
        })
    }
}

impl<R, A, C> SourceAcquisitionAuthorizationPortV1 for GitHubExternalSourceAcquisitionV1<R, A, C>
where
    R: GitHubCurrentBranchRemapper + Send + Sync,
    A: GitHubCanonicalReviewAnchorAuthorityV1
        + GitHubReviewBodyEvidenceAuthorityV1
        + Clone
        + Send
        + Sync,
    C: ConfigurationControlStore + Clone + Send + Sync,
{
    fn recheck<'a>(
        &'a self,
        task: &'a SourceScheduledRefetchV1,
        _phase: SourceAcquisitionAuthorizationPhaseV1,
    ) -> SourceAcquisitionFuture<'a, SourceAcquisitionAuthorizationOutcomeV1> {
        Box::pin(async move {
            if task.definition() != &self.definition
                || task.binding() != &self.binding
                || task.request() != &self.acquisition_request
            {
                return SourceAcquisitionAuthorizationOutcomeV1::Unauthorized;
            }
            self.current_grant(task).await
        })
    }
}

impl<R, A, C> SourceCanonicalRefetchPortV1 for GitHubExternalSourceAcquisitionV1<R, A, C>
where
    R: GitHubCurrentBranchRemapper + Send + Sync,
    A: GitHubCanonicalReviewAnchorAuthorityV1
        + GitHubReviewBodyEvidenceAuthorityV1
        + Clone
        + Send
        + Sync,
    C: ConfigurationControlStore + Clone + Send + Sync,
{
    fn refetch<'a>(
        &'a self,
        task: &'a SourceScheduledRefetchV1,
        grant: &'a SourceAcquisitionGrantV1,
        cancellation: &'a ObservationCancellation,
    ) -> SourceAcquisitionFuture<'a, SourceCanonicalRefetchOutcomeV1> {
        Box::pin(async move {
            self.refetch_page(task, grant, cancellation).await.map_or(
                SourceCanonicalRefetchOutcomeV1::Unavailable,
                SourceCanonicalRefetchOutcomeV1::Fetched,
            )
        })
    }
}

fn digest_suffix(digest: &ManifestDigest) -> Option<&str> {
    digest.as_str().strip_prefix("sha256:")
}

fn github_review_request(
    request: &SourceAcquisitionRequestV1,
) -> Option<GitHubReviewReadRequestV1> {
    match request {
        SourceAcquisitionRequestV1::GitHubReview {
            scope,
            operation,
            pull_request_id,
            ..
        } => Some(GitHubReviewReadRequestV1 {
            operation: *operation,
            scope: scope.clone(),
            pull_request_id: pull_request_id.clone(),
        }),
    }
}
