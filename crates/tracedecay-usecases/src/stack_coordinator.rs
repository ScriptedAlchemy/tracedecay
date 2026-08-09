//! Daemon-local read-only GitHub stacked-pull-request coordinator.
//!
//! The authenticated GitHub review owner supplies provider observations. This
//! coordinator pins them to exact project/repository/worktree/ref identities,
//! builds the canonical Plan 13 anchors, and serves deterministic bounded
//! multi-root fanout. It has no provider mutation or native-Git apply port.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tracedecay_application::ResolvedScope;
use tracedecay_domain::configuration::GitHubStackedPullRequestPolicyV1;
use tracedecay_domain::{
    CommitId, GitHubPullRequestIdV1, GitHubStackCapabilitySnapshotV1, GitHubStackCapabilityStateV1,
    GitHubStackLayerSnapshotV1, GitHubStackSnapshotV1, GitTopologyAnchorTargetV1,
    GitTopologySourceRoleV1, ManifestDigest, ObservationScopeV1, OrderedGitTopologySourceV1,
    PrivacyDomainBoundLocatorDigest, ProjectionGenerationId, ProviderId,
    PullRequestSnapshotAnchorRefV1, RefId, RetrievalAnchorId, UtcMicros, canonical_sha256,
    derive_git_topology_anchor_id,
};

pub const MAX_GITHUB_STACK_ROOTS_PER_FANOUT_V1: usize = 64;
pub const MAX_GITHUB_STACK_SIGNALS_PER_FANOUT_V1: usize = 128;
pub const MAX_GITHUB_STACK_LAYERS_V1: usize = 100;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubStackProviderLayerV1 {
    pub provider_position: u32,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub base_ref_id: RefId,
    pub head_ref_id: RefId,
    pub base_commit_id: CommitId,
    pub head_commit_id: CommitId,
    pub merge_base_commit_id: Option<CommitId>,
    pub protection_digest: ManifestDigest,
    pub ci_digest: ManifestDigest,
    pub merge_queue_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubStackProviderSnapshotV1 {
    pub response_digest: ManifestDigest,
    pub provider_stack_id_digest: ManifestDigest,
    pub final_target_ref_id: RefId,
    pub final_target_commit_id: CommitId,
    pub layers: Vec<GitHubStackProviderLayerV1>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubStackProviderOutcomeV1 {
    Unavailable,
    EnabledWithoutStack { response_digest: ManifestDigest },
    Enabled(GitHubStackProviderSnapshotV1),
    Degraded { response_digest: ManifestDigest },
}

impl GitHubStackProviderOutcomeV1 {
    fn response_digest(&self) -> Option<&ManifestDigest> {
        match self {
            Self::Unavailable => None,
            Self::EnabledWithoutStack { response_digest } | Self::Degraded { response_digest } => {
                Some(response_digest)
            }
            Self::Enabled(snapshot) => Some(&snapshot.response_digest),
        }
    }

    const fn capability_state(&self) -> GitHubStackCapabilityStateV1 {
        match self {
            Self::Unavailable => GitHubStackCapabilityStateV1::Unavailable,
            Self::EnabledWithoutStack { .. } | Self::Enabled(_) => {
                GitHubStackCapabilityStateV1::Enabled
            }
            Self::Degraded { .. } => GitHubStackCapabilityStateV1::Degraded,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubStackObservationV1 {
    pub scope: ResolvedScope,
    pub observed_at: UtcMicros,
    pub capability_anchor_id: RetrievalAnchorId,
    pub capability: GitHubStackCapabilitySnapshotV1,
    pub snapshot_anchor_id: Option<RetrievalAnchorId>,
    pub snapshot: Option<GitHubStackSnapshotV1>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHubStackCoordinatorErrorV1 {
    InvalidScope,
    InvalidProviderObservation,
    Poisoned,
}

#[derive(Default)]
pub struct DaemonGitHubStackCoordinatorV1 {
    policies: Mutex<BTreeMap<ManifestDigest, GitHubStackedPullRequestPolicyV1>>,
    observations: Mutex<BTreeMap<ManifestDigest, GitHubStackObservationV1>>,
}

impl DaemonGitHubStackCoordinatorV1 {
    pub fn register_scope(
        &self,
        scope: &ResolvedScope,
        policy: GitHubStackedPullRequestPolicyV1,
    ) -> Result<(), GitHubStackCoordinatorErrorV1> {
        scope
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidScope)?;
        self.policies
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?
            .insert(scope.scope_digest.clone(), policy);
        Ok(())
    }

    pub fn should_read_provider_stack(
        &self,
        scope: &ResolvedScope,
    ) -> Result<bool, GitHubStackCoordinatorErrorV1> {
        let policies = self
            .policies
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?;
        Ok(matches!(
            policies.get(&scope.scope_digest),
            Some(GitHubStackedPullRequestPolicyV1::ProbePrivatePreview)
        ))
    }

    pub fn observe_policy(
        &self,
        scope: ResolvedScope,
        provider: ProviderId,
        observed_at: UtcMicros,
    ) -> Result<GitHubStackObservationV1, GitHubStackCoordinatorErrorV1> {
        let state = if self.should_read_provider_stack(&scope)? {
            GitHubStackCapabilityStateV1::Unavailable
        } else {
            GitHubStackCapabilityStateV1::PrivatePreviewDisabled
        };
        if state == GitHubStackCapabilityStateV1::Unavailable
            && let Some(provider_observation) = self
                .observations
                .lock()
                .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?
                .get(&scope.scope_digest)
                .filter(|observation| {
                    matches!(
                        observation.capability.state,
                        GitHubStackCapabilityStateV1::Enabled
                            | GitHubStackCapabilityStateV1::Degraded
                    )
                })
                .cloned()
        {
            return Ok(provider_observation);
        }
        let digest = canonical_sha256(&(
            "tracedecay.github-stack.policy-observation.v1",
            &scope.scope_digest,
            state,
            observed_at,
        ))
        .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        self.store_observation(scope, provider, state, &digest, None, observed_at)
    }

    pub fn observe_provider(
        &self,
        scope: ResolvedScope,
        provider: ProviderId,
        outcome: GitHubStackProviderOutcomeV1,
        observed_at: UtcMicros,
    ) -> Result<GitHubStackObservationV1, GitHubStackCoordinatorErrorV1> {
        if !self.should_read_provider_stack(&scope)? {
            return self.observe_policy(scope, provider, observed_at);
        }
        let state = outcome.capability_state();
        let response_digest = match outcome.response_digest() {
            Some(digest) => digest.clone(),
            None => canonical_sha256(&(
                "tracedecay.github-stack.unavailable.v1",
                &scope.scope_digest,
                observed_at,
            ))
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?,
        };
        let snapshot = match outcome {
            GitHubStackProviderOutcomeV1::Enabled(snapshot) => Some(snapshot),
            _ => None,
        };
        self.store_observation(
            scope,
            provider,
            state,
            &response_digest,
            snapshot.as_ref(),
            observed_at,
        )
    }

    pub fn fanout_authorized<F>(
        &self,
        mut roots: Vec<ResolvedScope>,
        mut authorize: F,
    ) -> Result<Vec<GitHubStackObservationV1>, GitHubStackCoordinatorErrorV1>
    where
        F: FnMut(&ResolvedScope) -> bool,
    {
        roots.sort_by(|left, right| left.scope_digest.cmp(&right.scope_digest));
        roots.dedup_by(|left, right| left.scope_digest == right.scope_digest);
        roots.truncate(MAX_GITHUB_STACK_ROOTS_PER_FANOUT_V1);
        let observations = self
            .observations
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?;
        Ok(roots
            .into_iter()
            .filter(|root| authorize(root))
            .filter_map(|root| observations.get(&root.scope_digest).cloned())
            .take(MAX_GITHUB_STACK_SIGNALS_PER_FANOUT_V1)
            .collect())
    }

    fn store_observation(
        &self,
        scope: ResolvedScope,
        provider: ProviderId,
        state: GitHubStackCapabilityStateV1,
        response_digest: &ManifestDigest,
        provider_snapshot: Option<&GitHubStackProviderSnapshotV1>,
        observed_at: UtcMicros,
    ) -> Result<GitHubStackObservationV1, GitHubStackCoordinatorErrorV1> {
        scope
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidScope)?;
        provider
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        response_digest
            .validate()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        let source_anchor_id = response_anchor(&scope, response_digest)?;
        let generation_id = generation_id(response_digest)?;
        let capability = GitHubStackCapabilitySnapshotV1::new(
            provider.clone(),
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            state,
            generation_id.clone(),
            source_anchor_id,
        )
        .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        let owner = ObservationScopeV1::Project {
            project_id: scope.project_id.clone(),
        };
        let capability_anchor_id = derive_git_topology_anchor_id(
            &owner,
            &GitTopologyAnchorTargetV1::GitHubStackCapability(capability.clone()),
        )
        .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        let snapshot = provider_snapshot
            .map(|snapshot| {
                build_snapshot(&scope, &provider, &capability, &generation_id, snapshot)
            })
            .transpose()?;
        let snapshot_anchor_id = snapshot
            .as_ref()
            .map(|snapshot| {
                derive_git_topology_anchor_id(
                    &owner,
                    &GitTopologyAnchorTargetV1::GitHubStackSnapshot(snapshot.clone()),
                )
            })
            .transpose()
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
        let observation = GitHubStackObservationV1 {
            scope: scope.clone(),
            observed_at,
            capability_anchor_id,
            capability,
            snapshot_anchor_id,
            snapshot,
        };
        self.observations
            .lock()
            .map_err(|_| GitHubStackCoordinatorErrorV1::Poisoned)?
            .insert(scope.scope_digest, observation.clone());
        Ok(observation)
    }
}

fn build_snapshot(
    scope: &ResolvedScope,
    provider: &ProviderId,
    capability: &GitHubStackCapabilitySnapshotV1,
    generation_id: &ProjectionGenerationId,
    snapshot: &GitHubStackProviderSnapshotV1,
) -> Result<GitHubStackSnapshotV1, GitHubStackCoordinatorErrorV1> {
    if snapshot.layers.is_empty() || snapshot.layers.len() > MAX_GITHUB_STACK_LAYERS_V1 {
        return Err(GitHubStackCoordinatorErrorV1::InvalidProviderObservation);
    }
    let layers = snapshot
        .layers
        .iter()
        .map(|layer| {
            let merge_base_commit_id = layer
                .merge_base_commit_id
                .clone()
                .ok_or(GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
            let source_anchor_id = pull_request_anchor(scope, layer)?;
            let pull_request = PullRequestSnapshotAnchorRefV1 {
                provider: provider.clone(),
                project_id: scope.project_id.clone(),
                repository_id: scope.repository_id.clone(),
                worktree_id: scope.worktree_id.clone(),
                pull_request_id: layer.pull_request_id.clone(),
                base_commit_id: layer.base_commit_id.clone(),
                head_commit_id: layer.head_commit_id.clone(),
                merge_base_commit_id,
                source_anchor_id: source_anchor_id.clone(),
                snapshot_digest: canonical_sha256(&(
                    "tracedecay.github-stack.pull-request.v1",
                    &scope.scope_digest,
                    layer,
                ))
                .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?,
                sources: vec![OrderedGitTopologySourceV1 {
                    source_ordinal: 0,
                    role: GitTopologySourceRoleV1::PullRequestObservation,
                    anchor_id: source_anchor_id,
                }],
            };
            pull_request
                .validate()
                .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
            Ok(GitHubStackLayerSnapshotV1 {
                provider_position: layer.provider_position,
                pull_request,
                base_ref_id: layer.base_ref_id.clone(),
                head_ref_id: layer.head_ref_id.clone(),
                protection_digest: layer.protection_digest.clone(),
                ci_digest: layer.ci_digest.clone(),
                merge_queue_digest: layer.merge_queue_digest.clone(),
            })
        })
        .collect::<Result<Vec<_>, GitHubStackCoordinatorErrorV1>>()?;
    let provider_stack_id_digest = canonical_sha256(&(
        "tracedecay.github-stack.privacy-bound-provider-id.v1",
        &scope.scope_digest,
        &snapshot.provider_stack_id_digest,
    ))
    .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
    let provider_stack_id_digest =
        PrivacyDomainBoundLocatorDigest::new(provider_stack_id_digest.as_str())
            .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
    GitHubStackSnapshotV1::new(
        capability.clone(),
        provider_stack_id_digest,
        generation_id.clone(),
        snapshot.final_target_ref_id.clone(),
        snapshot.final_target_commit_id.clone(),
        layers,
        response_anchor(scope, &snapshot.response_digest)?,
    )
    .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)
}

fn response_anchor(
    scope: &ResolvedScope,
    digest: &ManifestDigest,
) -> Result<RetrievalAnchorId, GitHubStackCoordinatorErrorV1> {
    RetrievalAnchorId::new(format!(
        "anchor.github-stack.{}.{}",
        scope.repository_id.as_str(),
        digest.as_str().replace(':', ".")
    ))
    .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)
}

fn pull_request_anchor(
    scope: &ResolvedScope,
    layer: &GitHubStackProviderLayerV1,
) -> Result<RetrievalAnchorId, GitHubStackCoordinatorErrorV1> {
    let digest = canonical_sha256(&(
        "tracedecay.github-stack.pull-request-anchor.v1",
        &scope.scope_digest,
        layer,
    ))
    .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)?;
    response_anchor(scope, &digest)
}

fn generation_id(
    digest: &ManifestDigest,
) -> Result<ProjectionGenerationId, GitHubStackCoordinatorErrorV1> {
    ProjectionGenerationId::new(format!(
        "generation.github-stack.{}",
        digest.as_str().replace(':', ".")
    ))
    .map_err(|_| GitHubStackCoordinatorErrorV1::InvalidProviderObservation)
}
