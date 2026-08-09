//! Durable, authorization-bound GitHub stack retrieval anchors.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    FeedbackPortFuture, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
    GitHubReviewReadRequestV1,
};
use tracedecay_domain::feedback::FeedbackScopeV1;
use tracedecay_domain::{
    AccessPolicyDigest, AnchorDurabilityClass, AnchorLineageRefV3, AnchorOwnerBindingV1,
    AnchorProvenanceRelationV2, AnchorSourceGenerationV3, CapabilityId, CoverageReportV1,
    EvidenceClass, GitHubStackCapabilityStateV1, GitTopologyAnchorTargetV1,
    GitTopologySourceRoleV1, OrderedGitTopologySourceV1, PayloadAccessState,
    PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectionGenerationId, ProviderId,
    PullRequestSnapshotAnchorRefV1, RepositoryId, ResolutionAuthorizationV1, RetentionClass,
    RetrievalAnchorId, RetrievalAnchorRecordV3, RetrievalAnchorRecordV3Parts,
    RetrievalAnchorTargetV3, ScopeResolutionId, UserProfileId, UtcMicros, VectorWatermark,
    canonical_sha256,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_store::{RetrievalAnchorOwnerV1, StoredRetrievalAnchorRecordV1};
use tracedecay_tool_catalog::{CapabilityId as GrantCapabilityId, UseCaseId as GrantUseCaseId};

use super::stack::DecodedGitHubStackSnapshotV1;
use super::{GitHubProviderLifecycleV1, GitHubSourceAccessAuthorityV1};
use crate::advisory::context_matches_scope;
use crate::stack_coordinator::{
    GitHubStackObservationV1, GitHubStackProviderLayerV1, GitHubStackProviderOutcomeV1,
    GitHubStackProviderSnapshotV1, GitHubStackProviderSourceBindingV1,
};

const RETENTION_CLASS_V1: &str = "retention.github-stack.provider-evidence.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubStackAnchorPublicationOutcomeV1 {
    Published,
    Replayed,
    Denied,
    Stale,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubStackAnchorReadOutcomeV1 {
    Current(RetrievalAnchorRecordV3),
    Denied,
    Stale,
    Unavailable,
}

pub(super) trait GitHubStackReadAuthorityV1: Sync {
    fn bind_provider_snapshot(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
        provider: &ProviderId,
        decoded: DecodedGitHubStackSnapshotV1,
        merge_base_commit_ids: Vec<tracedecay_domain::CommitId>,
        observed_at: UtcMicros,
    ) -> Option<GitHubStackProviderSnapshotV1>;
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitHubStackDurableObservationV1 {
    pub observation: GitHubStackObservationV1,
    pub capability_anchor: RetrievalAnchorRecordV3,
    pub snapshot_anchor: Option<RetrievalAnchorRecordV3>,
}

#[derive(Clone)]
pub struct ProjectGitHubStackAnchorAuthorityV1 {
    database: Arc<RegisteredGlobalDb>,
    scope: FeedbackScopeV1,
}

impl ProjectGitHubStackAnchorAuthorityV1 {
    pub fn new(database: Arc<RegisteredGlobalDb>, scope: FeedbackScopeV1) -> Option<Self> {
        scope.validate().ok()?;
        (database.binding().shard_id.scope.project_id() == Some(&scope.project_id))
            .then_some(Self { database, scope })
    }

    pub fn privacy_domain_id(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
    ) -> Option<PrivacyDomainId> {
        self.request_admitted(context, request, tracedecay_application::now_micros())
            .then(|| {
                authorization(
                    &self.database.binding().shard_id.profile_id,
                    context,
                    request,
                )
            })
            .flatten()
            .map(|authorization| authorization.privacy_domain_id)
    }

    pub fn resolve_published_observation(
        database: &RegisteredGlobalDb,
        scope: &tracedecay_application::ResolvedScope,
        observation: GitHubStackObservationV1,
    ) -> Option<GitHubStackDurableObservationV1> {
        if observation.scope != *scope {
            return None;
        }
        let privacy_domain_id =
            privacy_domain_for_scope(&database.binding().shard_id.profile_id, scope)?;
        let owner = AnchorOwnerBindingV1::for_project(
            database.binding().shard_id.profile_id.clone(),
            scope.project_id.clone(),
            privacy_domain_id,
        )
        .ok()?;
        let capability_anchor = resolve_v3(database, &owner, &observation.capability_anchor_id)?;
        if capability_anchor.owner() != &owner
            || capability_anchor.target()
                != &RetrievalAnchorTargetV3::GitTopology(Box::new(
                    GitTopologyAnchorTargetV1::GitHubStackCapability(
                        observation.capability.clone(),
                    ),
                ))
        {
            return None;
        }
        let snapshot_anchor = match (&observation.snapshot, &observation.snapshot_anchor_id) {
            (Some(snapshot), Some(anchor_id)) => {
                let record = resolve_v3(database, &owner, anchor_id)?;
                if record.owner() != &owner
                    || record.target()
                        != &RetrievalAnchorTargetV3::GitTopology(Box::new(
                            GitTopologyAnchorTargetV1::GitHubStackSnapshot(snapshot.clone()),
                        ))
                {
                    return None;
                }
                Some(record)
            }
            (None, None) => None,
            _ => return None,
        };
        Some(GitHubStackDurableObservationV1 {
            observation,
            capability_anchor,
            snapshot_anchor,
        })
    }

    pub fn source_binding(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
        outcome: &GitHubStackProviderOutcomeV1,
        observed_at: UtcMicros,
    ) -> Option<GitHubStackProviderSourceBindingV1> {
        if !self.request_admitted(context, request, observed_at) {
            return None;
        }
        let (owner, authorization) = self.owner_and_authorization(context, request)?;
        let capability_source = exact_commit_source_record(
            &owner,
            &authorization,
            &request.scope.repository_id,
            &request.scope.head_commit_id,
            observed_at,
        )?;
        let snapshot_source_anchor_id = match outcome {
            GitHubStackProviderOutcomeV1::Enabled(snapshot) => {
                let selected = snapshot.layers.iter().find(|layer| {
                    layer.pull_request.pull_request_id == request.pull_request_id
                        && layer.pull_request.head_commit_id == request.scope.head_commit_id
                })?;
                Some(
                    retrieval_record(
                        owner.clone(),
                        GitTopologyAnchorTargetV1::PullRequestSnapshot(
                            selected.pull_request.clone(),
                        ),
                        observed_at,
                        authorization,
                    )?
                    .anchor_id()
                    .clone(),
                )
            }
            GitHubStackProviderOutcomeV1::Unavailable
            | GitHubStackProviderOutcomeV1::EnabledWithoutStack { .. }
            | GitHubStackProviderOutcomeV1::Degraded { .. } => None,
        };
        Some(GitHubStackProviderSourceBindingV1 {
            owner,
            capability_source_anchor_id: capability_source.anchor_id().clone(),
            snapshot_source_anchor_id,
        })
    }

    pub fn publish<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
        observation: &'a GitHubStackObservationV1,
        source_access: &'a dyn GitHubSourceAccessAuthorityV1,
    ) -> FeedbackPortFuture<'a, GitHubStackAnchorPublicationOutcomeV1> {
        Box::pin(async move {
            if !self.request_admitted(context, request, observation.observed_at)
                || !observation_matches_scope(observation, &self.scope)
            {
                return GitHubStackAnchorPublicationOutcomeV1::Denied;
            }
            if let Some(outcome) =
                blocked_publication(source_access.authorize(context, request).await)
            {
                return outcome;
            }
            let Some(records) = build_records(
                &self.database.binding().shard_id.profile_id,
                context,
                request,
                observation,
            ) else {
                return GitHubStackAnchorPublicationOutcomeV1::Unavailable;
            };
            let expected = records.clone();
            let publication = match self
                .database
                .publish_retrieval_anchor_records(records)
                .await
            {
                Ok(tracedecay_global_db::RetrievalAnchorPublicationOutcomeV1::Published) => {
                    GitHubStackAnchorPublicationOutcomeV1::Published
                }
                Ok(tracedecay_global_db::RetrievalAnchorPublicationOutcomeV1::Replayed) => {
                    GitHubStackAnchorPublicationOutcomeV1::Replayed
                }
                Err(_) => GitHubStackAnchorPublicationOutcomeV1::Unavailable,
            };
            if publication == GitHubStackAnchorPublicationOutcomeV1::Unavailable
                || !expected.iter().all(|candidate| {
                    self.database
                        .resolve_retrieval_anchor_record(
                            candidate.owner(),
                            candidate.anchor_id().clone(),
                        )
                        .ok()
                        .flatten()
                        .is_some_and(|resolved| semantic_replay(&resolved, candidate))
                })
            {
                return GitHubStackAnchorPublicationOutcomeV1::Unavailable;
            }
            if let Some(outcome) =
                blocked_publication(source_access.authorize(context, request).await)
            {
                return outcome;
            }
            publication
        })
    }

    pub fn resolve<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a GitHubReviewReadRequestV1,
        anchor_id: &'a RetrievalAnchorId,
        source_access: &'a dyn GitHubSourceAccessAuthorityV1,
    ) -> FeedbackPortFuture<'a, GitHubStackAnchorReadOutcomeV1> {
        Box::pin(async move {
            let observed_at = tracedecay_application::now_micros();
            if !self.request_admitted(context, request, observed_at)
                || anchor_id.validate().is_err()
            {
                return GitHubStackAnchorReadOutcomeV1::Denied;
            }
            if let Some(outcome) = blocked_read(source_access.authorize(context, request).await) {
                return outcome;
            }
            let Some(expected_authorization) = authorization(
                &self.database.binding().shard_id.profile_id,
                context,
                request,
            ) else {
                return GitHubStackAnchorReadOutcomeV1::Denied;
            };
            let Ok(owner) = AnchorOwnerBindingV1::for_project(
                self.database.binding().shard_id.profile_id.clone(),
                self.scope.project_id.clone(),
                expected_authorization.privacy_domain_id.clone(),
            ) else {
                return GitHubStackAnchorReadOutcomeV1::Denied;
            };
            let record = match self.database.resolve_retrieval_anchor_record(
                RetrievalAnchorOwnerV1::from(owner.clone()),
                anchor_id.clone(),
            ) {
                Ok(Some(StoredRetrievalAnchorRecordV1::V3(record))) => record,
                Ok(Some(StoredRetrievalAnchorRecordV1::V2(_))) | Ok(None) | Err(_) => {
                    return GitHubStackAnchorReadOutcomeV1::Unavailable;
                }
            };
            if record.owner() != &owner
                || !record_matches_scope(&record, &self.scope)
                || record.authorization() != &expected_authorization
            {
                return GitHubStackAnchorReadOutcomeV1::Denied;
            }
            if let Some(outcome) = blocked_read(source_access.authorize(context, request).await) {
                return outcome;
            }
            GitHubStackAnchorReadOutcomeV1::Current(record)
        })
    }

    fn owner_and_authorization(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
    ) -> Option<(AnchorOwnerBindingV1, ResolutionAuthorizationV1)> {
        let authorization = authorization(
            &self.database.binding().shard_id.profile_id,
            context,
            request,
        )?;
        let owner = AnchorOwnerBindingV1::for_project(
            self.database.binding().shard_id.profile_id.clone(),
            self.scope.project_id.clone(),
            authorization.privacy_domain_id.clone(),
        )
        .ok()?;
        Some((owner, authorization))
    }

    fn request_admitted(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
        observed_at: UtcMicros,
    ) -> bool {
        let (Ok(capability), Ok(use_case)) = (
            GrantCapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1),
            GrantUseCaseId::new(GITHUB_REVIEW_INGEST_USE_CASE_ID_V1),
        ) else {
            return false;
        };
        request.validate().is_ok()
            && request.scope == self.scope
            && context_matches_scope(context, &self.scope)
            && context.admission_at(observed_at)
                == tracedecay_application::RequestAdmission::Admitted
            && context.allows(&capability, &use_case)
    }
}

impl GitHubStackReadAuthorityV1 for ProjectGitHubStackAnchorAuthorityV1 {
    fn bind_provider_snapshot(
        &self,
        context: &RequestContext,
        request: &GitHubReviewReadRequestV1,
        provider: &ProviderId,
        decoded: DecodedGitHubStackSnapshotV1,
        merge_base_commit_ids: Vec<tracedecay_domain::CommitId>,
        observed_at: UtcMicros,
    ) -> Option<GitHubStackProviderSnapshotV1> {
        if !self.request_admitted(context, request, observed_at)
            || provider.validate().is_err()
            || decoded.layers.len() != merge_base_commit_ids.len()
        {
            return None;
        }
        let (owner, authorization) = self.owner_and_authorization(context, request)?;
        let mut selected_found = false;
        let layers = decoded
            .layers
            .into_iter()
            .zip(merge_base_commit_ids)
            .map(|(layer, merge_base_commit_id)| {
                let source = exact_commit_source_record(
                    &owner,
                    &authorization,
                    &request.scope.repository_id,
                    &layer.head_commit_id,
                    observed_at,
                )?;
                let snapshot_digest = canonical_sha256(&(
                    "tracedecay.github-stack.pull-request-snapshot.v1",
                    provider,
                    context.scope(),
                    &layer.pull_request_id,
                    layer.provider_position,
                    &layer.base_ref_id,
                    &layer.head_ref_id,
                    &layer.base_commit_id,
                    &layer.head_commit_id,
                    &merge_base_commit_id,
                    &decoded.response_digest,
                ))
                .ok()?;
                let pull_request = PullRequestSnapshotAnchorRefV1 {
                    provider: provider.clone(),
                    project_id: context.scope().project_id.clone(),
                    repository_id: context.scope().repository_id.clone(),
                    worktree_id: context.scope().worktree_id.clone(),
                    pull_request_id: layer.pull_request_id,
                    base_commit_id: layer.base_commit_id,
                    head_commit_id: layer.head_commit_id,
                    merge_base_commit_id,
                    source_anchor_id: source.anchor_id().clone(),
                    snapshot_digest,
                    sources: vec![OrderedGitTopologySourceV1 {
                        source_ordinal: 0,
                        role: GitTopologySourceRoleV1::PullRequestObservation,
                        anchor_id: source.anchor_id().clone(),
                    }],
                };
                pull_request.validate().ok()?;
                if pull_request.pull_request_id == request.pull_request_id {
                    if pull_request.head_commit_id != request.scope.head_commit_id {
                        return None;
                    }
                    selected_found = true;
                }
                Some(GitHubStackProviderLayerV1 {
                    provider_position: layer.provider_position,
                    pull_request,
                    base_ref_id: layer.base_ref_id,
                    head_ref_id: layer.head_ref_id,
                    protection_digest: layer.protection_digest,
                    ci_digest: layer.ci_digest,
                    merge_queue_digest: layer.merge_queue_digest,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        selected_found.then_some(GitHubStackProviderSnapshotV1 {
            response_digest: decoded.response_digest,
            provider_stack_id_digest: decoded.provider_stack_id_digest,
            final_target_ref_id: decoded.final_target_ref_id,
            final_target_commit_id: decoded.final_target_commit_id,
            layers,
        })
    }
}

fn resolve_v3(
    database: &RegisteredGlobalDb,
    owner: &AnchorOwnerBindingV1,
    anchor_id: &RetrievalAnchorId,
) -> Option<RetrievalAnchorRecordV3> {
    match database
        .resolve_retrieval_anchor_record(
            RetrievalAnchorOwnerV1::from(owner.clone()),
            anchor_id.clone(),
        )
        .ok()??
    {
        StoredRetrievalAnchorRecordV1::V3(record) => Some(record),
        StoredRetrievalAnchorRecordV1::V2(_) => None,
    }
}

fn exact_commit_source_record(
    owner: &AnchorOwnerBindingV1,
    authorization: &ResolutionAuthorizationV1,
    repository_id: &RepositoryId,
    commit_id: &tracedecay_domain::CommitId,
    ingested_at: UtcMicros,
) -> Option<RetrievalAnchorRecordV3> {
    let digest = canonical_sha256(&(
        "tracedecay.github-stack.provider-commit-source.v1",
        owner,
        repository_id,
        commit_id,
    ))
    .ok()?;
    let suffix = digest.as_str().strip_prefix("sha256:")?;
    let mut source_authorization = authorization.clone();
    source_authorization.canonical_request_digest =
        PrivacyDomainBoundLocatorDigest::new(digest.as_str()).ok()?;
    RetrievalAnchorRecordV3::new(RetrievalAnchorRecordV3Parts {
        target: RetrievalAnchorTargetV3::ExactRepositoryCommit {
            repository_id: repository_id.clone(),
            commit_id: commit_id.clone(),
        },
        owner: owner.clone(),
        aliases: Vec::new(),
        occurred_at: None,
        ingested_at,
        evidence_class: EvidenceClass::ProviderDeclared,
        source_generation: AnchorSourceGenerationV3::Unknown,
        projection_generation: ProjectionGenerationId::new(format!(
            "generation.github-stack-source.{suffix}"
        ))
        .ok()?,
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: Vec::new(),
        source_anchors: Vec::new(),
        authorization: source_authorization,
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new(RETENTION_CLASS_V1).ok()?,
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .ok()
}

fn privacy_domain_for_scope(
    profile_id: &UserProfileId,
    scope: &tracedecay_application::ResolvedScope,
) -> Option<PrivacyDomainId> {
    let scope_suffix = scope.scope_digest.as_str().strip_prefix("sha256:")?;
    PrivacyDomainId::new(format!(
        "privacy.github-stack.{}.{}",
        profile_id.as_str(),
        scope_suffix
    ))
    .ok()
}

fn semantic_replay(
    resolved: &StoredRetrievalAnchorRecordV1,
    expected: &StoredRetrievalAnchorRecordV1,
) -> bool {
    match (resolved, expected) {
        (
            StoredRetrievalAnchorRecordV1::V2(resolved),
            StoredRetrievalAnchorRecordV1::V2(expected),
        ) => resolved.is_semantic_replay_of(expected),
        (
            StoredRetrievalAnchorRecordV1::V3(resolved),
            StoredRetrievalAnchorRecordV1::V3(expected),
        ) => resolved.is_semantic_replay_of(expected),
        _ => resolved == expected,
    }
}

fn build_records(
    profile_id: &UserProfileId,
    context: &RequestContext,
    request: &GitHubReviewReadRequestV1,
    observation: &GitHubStackObservationV1,
) -> Option<Vec<StoredRetrievalAnchorRecordV1>> {
    let authorization = authorization(profile_id, context, request)?;
    let owner = AnchorOwnerBindingV1::for_project(
        profile_id.clone(),
        observation.scope.project_id.clone(),
        authorization.privacy_domain_id.clone(),
    )
    .ok()?;
    let mut records = BTreeMap::new();
    let capability_source = exact_commit_source_record(
        &owner,
        &authorization,
        &request.scope.repository_id,
        &request.scope.head_commit_id,
        observation.observed_at,
    )?;
    if capability_source.anchor_id() != &observation.capability.source_anchor_id {
        return None;
    }
    insert_record(
        &mut records,
        StoredRetrievalAnchorRecordV1::V3(capability_source),
    )?;
    if let Some(snapshot) = &observation.snapshot {
        for layer in &snapshot.layers {
            let source = exact_commit_source_record(
                &owner,
                &authorization,
                &request.scope.repository_id,
                &layer.pull_request.head_commit_id,
                observation.observed_at,
            )?;
            if source.anchor_id() != &layer.pull_request.source_anchor_id {
                return None;
            }
            insert_record(&mut records, StoredRetrievalAnchorRecordV1::V3(source))?;
            let pull_request = retrieval_record(
                owner.clone(),
                GitTopologyAnchorTargetV1::PullRequestSnapshot(layer.pull_request.clone()),
                observation.observed_at,
                authorization.clone(),
            )?;
            insert_record(
                &mut records,
                StoredRetrievalAnchorRecordV1::V3(pull_request),
            )?;
        }
        if !records.contains_key(&snapshot.source_anchor_id) {
            return None;
        }
    }
    let capability_target =
        GitTopologyAnchorTargetV1::GitHubStackCapability(observation.capability.clone());
    let capability = retrieval_record(
        owner.clone(),
        capability_target,
        observation.observed_at,
        authorization.clone(),
    )?;
    if capability.anchor_id() != &observation.capability_anchor_id {
        return None;
    }
    insert_record(&mut records, StoredRetrievalAnchorRecordV1::V3(capability))?;
    match (&observation.snapshot, &observation.snapshot_anchor_id) {
        (Some(snapshot), Some(_))
            if observation.capability.state == GitHubStackCapabilityStateV1::Enabled =>
        {
            let snapshot_record = retrieval_record(
                owner.clone(),
                GitTopologyAnchorTargetV1::GitHubStackSnapshot(snapshot.clone()),
                observation.observed_at,
                authorization.clone(),
            )?;
            if snapshot_record.anchor_id() != observation.snapshot_anchor_id.as_ref()? {
                return None;
            }
            insert_record(
                &mut records,
                StoredRetrievalAnchorRecordV1::V3(snapshot_record),
            )?;
        }
        (None, None) => {}
        _ => return None,
    }
    let records = records.into_values().collect::<Vec<_>>();
    let published_ids = records
        .iter()
        .map(|record| record.anchor_id())
        .collect::<BTreeSet<_>>();
    records
        .iter()
        .all(|record| match record {
            StoredRetrievalAnchorRecordV1::V3(record) => record
                .source_anchors()
                .iter()
                .all(|source| published_ids.contains(source.anchor_id())),
            StoredRetrievalAnchorRecordV1::V2(_) => false,
        })
        .then_some(records)
}

fn insert_record(
    records: &mut BTreeMap<RetrievalAnchorId, StoredRetrievalAnchorRecordV1>,
    record: StoredRetrievalAnchorRecordV1,
) -> Option<()> {
    match records.get(record.anchor_id()) {
        Some(existing) if semantic_replay(existing, &record) => Some(()),
        Some(_) => None,
        None => {
            records.insert(record.anchor_id().clone(), record);
            Some(())
        }
    }
}

fn retrieval_record(
    owner: AnchorOwnerBindingV1,
    target: GitTopologyAnchorTargetV1,
    ingested_at: UtcMicros,
    authorization: ResolutionAuthorizationV1,
) -> Option<RetrievalAnchorRecordV3> {
    let mut seen = BTreeSet::new();
    let source_anchors = target
        .ordered_sources()
        .iter()
        .filter(|source| seen.insert(source.anchor_id.clone()))
        .enumerate()
        .map(|(ordinal, source)| {
            AnchorLineageRefV3::new(
                u64::try_from(ordinal).ok()?,
                AnchorProvenanceRelationV2::Observed,
                source.anchor_id.clone(),
                owner.clone(),
            )
            .ok()
        })
        .collect::<Option<Vec<_>>>()?;
    let source_generation = AnchorSourceGenerationV3::GitTopology(target.generation());
    let projection_generation = match &source_generation {
        AnchorSourceGenerationV3::GitTopology(
            tracedecay_domain::GitTopologyGenerationRefV1::GitHubStackCapability {
                generation_id,
                ..
            }
            | tracedecay_domain::GitTopologyGenerationRefV1::GitHubStackSnapshot {
                generation_id,
                ..
            },
        ) => generation_id.clone(),
        AnchorSourceGenerationV3::GitTopology(
            tracedecay_domain::GitTopologyGenerationRefV1::ProviderCommit {
                source_anchor_id,
                commit_id,
            },
        ) => {
            let digest = canonical_sha256(&(
                "tracedecay.github-stack.pull-request-projection.v1",
                source_anchor_id,
                commit_id,
            ))
            .ok()?;
            ProjectionGenerationId::new(format!(
                "generation.github-pull-request.{}",
                digest.as_str().strip_prefix("sha256:")?
            ))
            .ok()?
        }
        _ => return None,
    };
    RetrievalAnchorRecordV3::new(RetrievalAnchorRecordV3Parts {
        target: RetrievalAnchorTargetV3::GitTopology(Box::new(target)),
        owner,
        aliases: Vec::new(),
        occurred_at: None,
        ingested_at,
        evidence_class: EvidenceClass::ProviderDeclared,
        source_generation,
        projection_generation,
        projection_watermark: VectorWatermark::default(),
        coverage: CoverageReportV1::default(),
        source_observations: Vec::new(),
        source_anchors,
        authorization,
        payload_access: PayloadAccessState::Eligible,
        retention_class: RetentionClass::new(RETENTION_CLASS_V1).ok()?,
        durability: AnchorDurabilityClass::DurableEvidence,
    })
    .ok()
}

fn authorization(
    profile_id: &UserProfileId,
    context: &RequestContext,
    request: &GitHubReviewReadRequestV1,
) -> Option<ResolutionAuthorizationV1> {
    let scope_suffix = context
        .scope()
        .scope_digest
        .as_str()
        .strip_prefix("sha256:")?;
    let policy = canonical_sha256(&(
        "tracedecay.github-stack.anchor-policy.v1",
        context.scope(),
        GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
    ))
    .ok()?;
    let request_digest = canonical_sha256(&(
        "tracedecay.github-stack.anchor-request.v1",
        context.scope(),
        &request.scope,
        &request.pull_request_id,
    ))
    .ok()?;
    Some(ResolutionAuthorizationV1 {
        resolved_scope_id: ScopeResolutionId::new(format!("scope.github-stack.{scope_suffix}"))
            .ok()?,
        privacy_domain_id: privacy_domain_for_scope(profile_id, context.scope())?,
        access_policy_digest: AccessPolicyDigest::new(policy.as_str()).ok()?,
        capability_id: CapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1).ok()?,
        canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(request_digest.as_str())
            .ok()?,
    })
}

fn observation_matches_scope(
    observation: &GitHubStackObservationV1,
    scope: &FeedbackScopeV1,
) -> bool {
    let Ok(branch_ref) = tracedecay_domain::RefId::new(scope.branch_ref.clone()) else {
        return false;
    };
    observation.scope.project_id == scope.project_id
        && observation.scope.repository_id == scope.repository_id
        && observation.scope.worktree_id == scope.worktree_id
        && observation.scope.reference.as_ref() == Some(&branch_ref)
        && observation.capability.project_id == scope.project_id
        && observation.capability.repository_id == scope.repository_id
        && observation.capability.worktree_id == scope.worktree_id
}

fn record_matches_scope(record: &RetrievalAnchorRecordV3, scope: &FeedbackScopeV1) -> bool {
    let RetrievalAnchorTargetV3::GitTopology(target) = record.target() else {
        return false;
    };
    target.project_id() == &scope.project_id && target.repository_id() == &scope.repository_id
}

const fn blocked_publication(
    lifecycle: GitHubProviderLifecycleV1,
) -> Option<GitHubStackAnchorPublicationOutcomeV1> {
    match lifecycle {
        GitHubProviderLifecycleV1::Ready => None,
        GitHubProviderLifecycleV1::Stale => Some(GitHubStackAnchorPublicationOutcomeV1::Stale),
        GitHubProviderLifecycleV1::Denied | GitHubProviderLifecycleV1::Ambiguous => {
            Some(GitHubStackAnchorPublicationOutcomeV1::Denied)
        }
        GitHubProviderLifecycleV1::Unavailable => {
            Some(GitHubStackAnchorPublicationOutcomeV1::Unavailable)
        }
    }
}

const fn blocked_read(
    lifecycle: GitHubProviderLifecycleV1,
) -> Option<GitHubStackAnchorReadOutcomeV1> {
    match lifecycle {
        GitHubProviderLifecycleV1::Ready => None,
        GitHubProviderLifecycleV1::Stale => Some(GitHubStackAnchorReadOutcomeV1::Stale),
        GitHubProviderLifecycleV1::Denied | GitHubProviderLifecycleV1::Ambiguous => {
            Some(GitHubStackAnchorReadOutcomeV1::Denied)
        }
        GitHubProviderLifecycleV1::Unavailable => Some(GitHubStackAnchorReadOutcomeV1::Unavailable),
    }
}
