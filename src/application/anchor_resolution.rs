//! Typed evidence-anchor resolution reports through the daemon authority.
//!
//! Resolution is always reported as one of the seven typed
//! [`AnchorResolutionStateV2`] states with coverage, watermark drift, and the
//! authorization that bounded the decision; it is never a bare record or a
//! bare absence. The resolver rechecks the caller's authorization on every
//! call, so possessing an anchor id never grants access and never leaks an
//! unauthorized target's existence. A returned record always keeps its frozen
//! owner, target, and source generation: resolution never silently switches
//! owner, provider, project, session variant, or source generation.

use std::collections::BTreeSet;
use std::future::Future;

use serde::Serialize;
use tracedecay_domain::{
    AnchorLineageRefV2, AnchorProvenanceRelationV2, AnchorResolutionStateV2,
    AnchorSourceGenerationV2, ApplyReceiptAnchorRefV1, AuthorizedAnchorResolutionV2,
    CheckSnapshotAnchorRefV1, CiFailureLocalizationResultV1, ConflictEvidenceAnchorRefV1,
    CoverageReportV1, DomainError, FactOwnerV1, FrozenWatermarkResolutionV1,
    GenerationBoundRepositoryProvenanceV1, GitHubReviewIngressResultV1, GitHubReviewItemV1,
    GitIndexPreviewV1, GitIndexTransactionReceiptV1, GitTopologyAnchorTargetV1,
    GitTopologyGenerationRefV1, IntegrationReceiptAnchorRefV1, NativeGitObjectAnchorRefV1,
    ObservationScopeV1, PayloadAccessState, PreflightPreviewAnchorRefV1,
    PullRequestSnapshotAnchorRefV1, RefSnapshotAnchorRefV1, RepositoryCaptureAnchorRefV1,
    RepositoryStateSnapshotV1, ResolutionAuthorizationV1, RetrievalAnchorId,
    RetrievalAnchorRecordV2, RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2,
    ReviewSnapshotAnchorRefV1, VectorWatermark, canonical_sha256, derive_git_topology_anchor_id,
};
use tracedecay_store::ObservedEvidenceAnchorResolution;

use crate::application::memory::EvidenceAnchorResolutionError;

/// Canonical digest domain for record-less resolution markers. The digest
/// binds only the requested anchor id and the typed state; it never embeds
/// payload bytes, a query, or a source locator.
const UNRESOLVED_ANCHOR_DIGEST_DOMAIN: &str = "tracedecay.observation-anchor.unresolved.v1";

#[derive(Serialize)]
struct UnresolvedAnchorDigestV1<'a> {
    domain: &'static str,
    anchor_id: &'a RetrievalAnchorId,
    state: AnchorResolutionStateV2,
}

/// Typed outcome of resolving one evidence anchor through the daemon
/// authority. The [`AuthorizedAnchorResolutionV2`] metadata is validated by
/// the domain contract; the retained record is present exactly when the store
/// resolved a single authoritative record, whatever its declared payload
/// access.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceAnchorResolutionReport {
    resolution: AuthorizedAnchorResolutionV2,
    record: Option<RetrievalAnchorRecordV2>,
}

impl EvidenceAnchorResolutionReport {
    /// Compose a validated typed report from the store's observation. The
    /// `absent_authorization` bounds record-less resolutions (unavailable and
    /// ambiguous bindings) and is ignored when the retained record carries
    /// its own frozen authorization.
    pub fn from_observation(
        anchor_id: RetrievalAnchorId,
        observed: ObservedEvidenceAnchorResolution,
        absent_authorization: ResolutionAuthorizationV1,
    ) -> Result<Self, DomainError> {
        anchor_id.validate()?;
        match observed {
            ObservedEvidenceAnchorResolution::Resolved {
                record,
                observed_watermark,
            } => {
                if record.anchor_id() != &anchor_id {
                    return Err(DomainError::UnknownReference {
                        field: "resolved anchor identity",
                    });
                }
                record.validate()?;
                let watermark = FrozenWatermarkResolutionV1::new(
                    record.projection_watermark().clone(),
                    observed_watermark,
                );
                let state =
                    AnchorResolutionStateV2::classify(record.payload_access(), watermark.drift);
                let resolution = AuthorizedAnchorResolutionV2::new(
                    anchor_id,
                    record.authorization().clone(),
                    watermark,
                    record.coverage().clone(),
                    state,
                    record.payload_access(),
                    canonical_sha256(&record)?,
                )?;
                Ok(Self {
                    resolution,
                    record: Some(*record),
                })
            }
            ObservedEvidenceAnchorResolution::Unavailable => Self::unresolved(
                anchor_id,
                absent_authorization,
                AnchorResolutionStateV2::Unavailable,
                PayloadAccessState::Unavailable,
            ),
            ObservedEvidenceAnchorResolution::Ambiguous => Self::unresolved(
                anchor_id,
                absent_authorization,
                AnchorResolutionStateV2::Ambiguous,
                PayloadAccessState::Ambiguous,
            ),
        }
    }

    fn unresolved(
        anchor_id: RetrievalAnchorId,
        authorization: ResolutionAuthorizationV1,
        state: AnchorResolutionStateV2,
        payload_access: PayloadAccessState,
    ) -> Result<Self, DomainError> {
        authorization.validate()?;
        let watermark = FrozenWatermarkResolutionV1::new(
            VectorWatermark::default(),
            VectorWatermark::default(),
        );
        let resolved_record_digest = canonical_sha256(&UnresolvedAnchorDigestV1 {
            domain: UNRESOLVED_ANCHOR_DIGEST_DOMAIN,
            anchor_id: &anchor_id,
            state,
        })?;
        let resolution = AuthorizedAnchorResolutionV2::new(
            anchor_id,
            authorization,
            watermark,
            CoverageReportV1::default(),
            state,
            payload_access,
            resolved_record_digest,
        )?;
        Ok(Self {
            resolution,
            record: None,
        })
    }

    /// Validated payload-free resolution metadata: state, coverage, watermark
    /// drift, and the bounding authorization.
    pub fn resolution(&self) -> &AuthorizedAnchorResolutionV2 {
        &self.resolution
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        self.resolution.anchor_id()
    }

    pub fn state(&self) -> AnchorResolutionStateV2 {
        self.resolution.state()
    }

    /// The single authoritative retained record, when the store resolved one.
    /// The record is immutable metadata; its declared `payload_access` says
    /// whether the retained payload may be accessed.
    pub fn record(&self) -> Option<&RetrievalAnchorRecordV2> {
        self.record.as_ref()
    }
}

/// Canonical application of one topology target to the existing append-only
/// retrieval-anchor record. Ordered source roles remain in the target while
/// the existing lineage collection retains one owner-bound edge per source
/// anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitTopologyAnchorApplicationV1 {
    target: GitTopologyAnchorTargetV1,
    source_generation: GitTopologyGenerationRefV1,
    source_anchors: Vec<AnchorLineageRefV2>,
}

impl GitTopologyAnchorApplicationV1 {
    pub fn new(
        owner: ObservationScopeV1,
        target: GitTopologyAnchorTargetV1,
    ) -> Result<Self, DomainError> {
        target.validate()?;
        let ObservationScopeV1::Project { project_id } = &owner else {
            return Err(DomainError::UnknownReference {
                field: "git topology application owner",
            });
        };
        if project_id != target.project_id() {
            return Err(DomainError::UnknownReference {
                field: "git topology application project",
            });
        }

        let mut seen = BTreeSet::new();
        let mut source_anchors = Vec::new();
        for source in target.ordered_sources() {
            if seen.insert(source.anchor_id.clone()) {
                source_anchors.push(AnchorLineageRefV2::new(
                    AnchorProvenanceRelationV2::DerivedFrom,
                    source.anchor_id.clone(),
                    owner.clone(),
                )?);
            }
        }
        source_anchors.sort_unstable();
        Ok(Self {
            source_generation: target.generation(),
            target,
            source_anchors,
        })
    }

    pub fn target(&self) -> &GitTopologyAnchorTargetV1 {
        &self.target
    }

    pub fn source_generation(&self) -> &GitTopologyGenerationRefV1 {
        &self.source_generation
    }

    pub fn source_anchors(&self) -> &[AnchorLineageRefV2] {
        &self.source_anchors
    }

    /// Replace only target-owned fields in constructor material. Authorization,
    /// retention, projection watermark, and evidence metadata stay with the
    /// caller's already-authorized publication transaction.
    pub fn apply_to(
        &self,
        mut parts: RetrievalAnchorRecordV2Parts,
    ) -> Result<RetrievalAnchorRecordV2Parts, DomainError> {
        if let ObservationScopeV1::Project { project_id } = &parts.owner {
            if project_id != self.target.project_id() {
                return Err(DomainError::UnknownReference {
                    field: "git topology publication project",
                });
            }
        } else {
            return Err(DomainError::UnknownReference {
                field: "git topology publication owner",
            });
        }
        parts.target = RetrievalAnchorTargetV2::GitTopology(Box::new(self.target.clone()));
        parts.source_generation =
            AnchorSourceGenerationV2::GitTopology(self.source_generation.clone());
        parts.source_anchors.clone_from(&self.source_anchors);
        Ok(parts)
    }
}

pub fn apply_worktree_snapshot_anchor(
    owner: ObservationScopeV1,
    provenance: &GenerationBoundRepositoryProvenanceV1,
    snapshot: &RepositoryStateSnapshotV1,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    let repository = RepositoryCaptureAnchorRefV1::new(provenance, snapshot)?;
    let target = tracedecay_domain::WorktreeCaptureAnchorRefV1::new(repository)?;
    GitTopologyAnchorApplicationV1::new(owner, GitTopologyAnchorTargetV1::WorktreeCapture(target))
}

pub fn apply_repository_snapshot_anchor(
    owner: ObservationScopeV1,
    provenance: &GenerationBoundRepositoryProvenanceV1,
    snapshot: &RepositoryStateSnapshotV1,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    let target = RepositoryCaptureAnchorRefV1::new(provenance, snapshot)?;
    GitTopologyAnchorApplicationV1::new(owner, GitTopologyAnchorTargetV1::RepositoryCapture(target))
}

pub fn apply_ref_snapshot_anchor(
    owner: ObservationScopeV1,
    target: RefSnapshotAnchorRefV1,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    GitTopologyAnchorApplicationV1::new(owner, GitTopologyAnchorTargetV1::RefSnapshot(target))
}

pub fn apply_native_object_anchor(
    owner: ObservationScopeV1,
    target: NativeGitObjectAnchorRefV1,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    GitTopologyAnchorApplicationV1::new(owner, GitTopologyAnchorTargetV1::NativeObject(target))
}

pub fn apply_pull_request_anchor(
    owner: ObservationScopeV1,
    ingress: &GitHubReviewIngressResultV1,
    source_anchor_id: RetrievalAnchorId,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    let target = PullRequestSnapshotAnchorRefV1::from_ingress(ingress, source_anchor_id)?;
    GitTopologyAnchorApplicationV1::new(
        owner,
        GitTopologyAnchorTargetV1::PullRequestSnapshot(target),
    )
}

pub fn apply_review_anchor(
    owner: ObservationScopeV1,
    ingress: &GitHubReviewIngressResultV1,
    item: &GitHubReviewItemV1,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    let pull_request =
        PullRequestSnapshotAnchorRefV1::from_ingress(ingress, item.body_anchor.clone())?;
    let review = ReviewSnapshotAnchorRefV1::from_item(pull_request, item)?;
    GitTopologyAnchorApplicationV1::new(owner, GitTopologyAnchorTargetV1::ReviewSnapshot(review))
}

pub fn apply_check_anchor(
    owner: ObservationScopeV1,
    result: &CiFailureLocalizationResultV1,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    let target = CheckSnapshotAnchorRefV1::from_localization(result)?;
    GitTopologyAnchorApplicationV1::new(owner, GitTopologyAnchorTargetV1::CheckSnapshot(target))
}

pub fn apply_conflict_anchor(
    owner: ObservationScopeV1,
    repository: RepositoryCaptureAnchorRefV1,
    snapshot: &RepositoryStateSnapshotV1,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    let target = ConflictEvidenceAnchorRefV1::new(repository, snapshot)?;
    GitTopologyAnchorApplicationV1::new(owner, GitTopologyAnchorTargetV1::ConflictEvidence(target))
}

pub fn apply_preflight_anchor(
    owner: ObservationScopeV1,
    repository: RepositoryCaptureAnchorRefV1,
    preview: &GitIndexPreviewV1,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    let target = PreflightPreviewAnchorRefV1::new(repository, preview)?;
    GitTopologyAnchorApplicationV1::new(owner, GitTopologyAnchorTargetV1::PreflightPreview(target))
}

pub fn apply_git_receipt_anchor(
    owner: ObservationScopeV1,
    preflight: PreflightPreviewAnchorRefV1,
    preflight_anchor_id: RetrievalAnchorId,
    receipt: &GitIndexTransactionReceiptV1,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    let target = ApplyReceiptAnchorRefV1::new(preflight, preflight_anchor_id, receipt)?;
    GitTopologyAnchorApplicationV1::new(owner, GitTopologyAnchorTargetV1::ApplyReceipt(target))
}

pub fn apply_integration_receipt_anchor(
    owner: ObservationScopeV1,
    apply: ApplyReceiptAnchorRefV1,
    additional_sources: Vec<(
        tracedecay_domain::GitTopologySourceRoleV1,
        RetrievalAnchorId,
    )>,
) -> Result<GitTopologyAnchorApplicationV1, DomainError> {
    let target = IntegrationReceiptAnchorRefV1::new(apply, additional_sources)?;
    GitTopologyAnchorApplicationV1::new(
        owner,
        GitTopologyAnchorTargetV1::IntegrationReceipt(target),
    )
}

/// Freshness of an immutable topology target relative to later observations.
/// Retargeting never changes the anchored target id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitTopologyResolutionStateV1 {
    Current,
    Stale {
        anchored_generation: GitTopologyGenerationRefV1,
        resolution_state: AnchorResolutionStateV2,
    },
    Retargeted {
        anchored_target_id: RetrievalAnchorId,
        observed_target_id: RetrievalAnchorId,
    },
    SourceDisposition {
        source_anchor_id: RetrievalAnchorId,
        state: AnchorResolutionStateV2,
    },
}

/// Lossless authorized drilldown. Every source report is retained in the
/// target's exact ordinal order and is independently authorization-bounded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizedGitTopologyDrilldownV1 {
    root: EvidenceAnchorResolutionReport,
    sources: Vec<EvidenceAnchorResolutionReport>,
    state: GitTopologyResolutionStateV1,
}

impl AuthorizedGitTopologyDrilldownV1 {
    pub fn new(
        root: EvidenceAnchorResolutionReport,
        sources: Vec<EvidenceAnchorResolutionReport>,
        observed_target: Option<&GitTopologyAnchorTargetV1>,
    ) -> Result<Self, DomainError> {
        let record = root.record().ok_or(DomainError::UnknownReference {
            field: "git topology root record",
        })?;
        let RetrievalAnchorTargetV2::GitTopology(target) = record.target() else {
            return Err(DomainError::UnknownReference {
                field: "git topology root target",
            });
        };
        if sources.len() != target.ordered_sources().len() {
            return Err(DomainError::UnknownReference {
                field: "git topology drilldown source count",
            });
        }

        for (expected, source) in target.ordered_sources().iter().zip(&sources) {
            if source.anchor_id() != &expected.anchor_id {
                return Err(DomainError::UnknownReference {
                    field: "git topology drilldown source order",
                });
            }
            let root_authorization = root.resolution().authorization();
            let source_authorization = source.resolution().authorization();
            if root_authorization.resolved_scope_id != source_authorization.resolved_scope_id
                || root_authorization.privacy_domain_id != source_authorization.privacy_domain_id
            {
                return Err(DomainError::UnknownReference {
                    field: "git topology drilldown authorization",
                });
            }
        }

        let state = if root.state() != AnchorResolutionStateV2::Current {
            GitTopologyResolutionStateV1::Stale {
                anchored_generation: target.generation(),
                resolution_state: root.state(),
            }
        } else if let Some(source) = sources
            .iter()
            .find(|source| source.state() != AnchorResolutionStateV2::Current)
        {
            GitTopologyResolutionStateV1::SourceDisposition {
                source_anchor_id: source.anchor_id().clone(),
                state: source.state(),
            }
        } else if let Some(observed_target) = observed_target {
            observed_target.validate()?;
            if observed_target.repository_id() != target.repository_id()
                || observed_target.project_id() != target.project_id()
            {
                return Err(DomainError::SnapshotMismatch {
                    field: "git topology observed repository",
                });
            }
            if observed_target == &**target {
                GitTopologyResolutionStateV1::Current
            } else {
                GitTopologyResolutionStateV1::Retargeted {
                    anchored_target_id: root.anchor_id().clone(),
                    observed_target_id: derive_git_topology_anchor_id(
                        record.owner(),
                        observed_target,
                    )?,
                }
            }
        } else {
            GitTopologyResolutionStateV1::Current
        };

        Ok(Self {
            root,
            sources,
            state,
        })
    }

    pub fn root(&self) -> &EvidenceAnchorResolutionReport {
        &self.root
    }

    pub fn sources(&self) -> &[EvidenceAnchorResolutionReport] {
        &self.sources
    }

    pub fn state(&self) -> &GitTopologyResolutionStateV1 {
        &self.state
    }
}

/// Daemon/ingress-only boundary for typed evidence-anchor resolution.
/// Implementations must recheck the caller's authorization on every call and
/// must not expose a database handle.
pub trait EvidenceAnchorReportResolver: Send + Sync {
    fn resolve_evidence_anchor_report(
        &self,
        owner: FactOwnerV1,
        anchor_id: RetrievalAnchorId,
    ) -> impl Future<Output = Result<EvidenceAnchorResolutionReport, EvidenceAnchorResolutionError>> + Send;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tracedecay_domain::{
        AccessPolicyDigest, AnchorDurabilityClass, AnchorSourceGenerationV2,
        CanonicalObservationIdV1, CapabilityId, CommitId, EvidenceClass, GitTopologySourceRoleV1,
        ManifestDigest, ObservationScopeV1, OrderedGitTopologySourceV1,
        PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectId, ProjectionGenerationId,
        ProviderId, PullRequestSnapshotAnchorRefV1, RetentionClass, RetrievalAnchorRecordV2Parts,
        RetrievalAnchorTargetV2, ScopeResolutionId, ShardId, UtcMicros, WatermarkDriftV1,
        WorktreeId,
    };

    use super::*;

    const SHA256_FIXTURE: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn authorization() -> ResolutionAuthorizationV1 {
        ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
            privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
            access_policy_digest: AccessPolicyDigest::new(SHA256_FIXTURE).unwrap(),
            capability_id: CapabilityId::new("capability.fixture").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(SHA256_FIXTURE).unwrap(),
        }
    }

    fn watermark(components: &[(&str, u64)]) -> VectorWatermark {
        VectorWatermark {
            components: components
                .iter()
                .map(|(shard, sequence)| (ShardId::new(*shard).unwrap(), *sequence))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn record_with_access(payload_access: PayloadAccessState) -> RetrievalAnchorRecordV2 {
        let observation_id = CanonicalObservationIdV1::new(SHA256_FIXTURE).unwrap();
        RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
            target: RetrievalAnchorTargetV2::ExactObservation(observation_id.clone()),
            owner: ObservationScopeV1::Profile,
            aliases: vec![],
            occurred_at: None,
            ingested_at: UtcMicros(1),
            evidence_class: EvidenceClass::Observed,
            source_generation: AnchorSourceGenerationV2::Observation(
                tracedecay_domain::ObservationSourceGenerationV1::new(1).unwrap(),
            ),
            projection_generation: ProjectionGenerationId::new("projection.fixture.v1").unwrap(),
            projection_watermark: watermark(&[("observation.projection", 1)]),
            coverage: CoverageReportV1::default(),
            source_observations: vec![observation_id],
            source_anchors: vec![],
            authorization: authorization(),
            payload_access,
            retention_class: RetentionClass::new("retention.fixture").unwrap(),
            durability: AnchorDurabilityClass::DurableEvidence,
        })
        .unwrap()
    }

    fn report_for(
        access: PayloadAccessState,
        observed_watermark: VectorWatermark,
    ) -> EvidenceAnchorResolutionReport {
        let record = record_with_access(access);
        EvidenceAnchorResolutionReport::from_observation(
            record.anchor_id().clone(),
            ObservedEvidenceAnchorResolution::Resolved {
                record: Box::new(record),
                observed_watermark,
            },
            authorization(),
        )
        .unwrap()
    }

    fn project_observation_record_with_access(
        seed: char,
        payload_access: PayloadAccessState,
    ) -> RetrievalAnchorRecordV2 {
        let observation_id = CanonicalObservationIdV1::new(format!(
            "sha256:{}",
            std::iter::repeat_n(seed, 64).collect::<String>()
        ))
        .unwrap();
        RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
            target: RetrievalAnchorTargetV2::ExactObservation(observation_id.clone()),
            owner: ObservationScopeV1::Project {
                project_id: ProjectId::new("project.fixture").unwrap(),
            },
            aliases: vec![],
            occurred_at: None,
            ingested_at: UtcMicros(1),
            evidence_class: EvidenceClass::Observed,
            source_generation: AnchorSourceGenerationV2::Observation(
                tracedecay_domain::ObservationSourceGenerationV1::new(1).unwrap(),
            ),
            projection_generation: ProjectionGenerationId::new("projection.fixture.v1").unwrap(),
            projection_watermark: watermark(&[("observation.projection", 1)]),
            coverage: CoverageReportV1::default(),
            source_observations: vec![observation_id],
            source_anchors: vec![],
            authorization: authorization(),
            payload_access,
            retention_class: RetentionClass::new("retention.fixture").unwrap(),
            durability: AnchorDurabilityClass::DurableEvidence,
        })
        .unwrap()
    }

    fn project_observation_record(seed: char) -> RetrievalAnchorRecordV2 {
        project_observation_record_with_access(seed, PayloadAccessState::Eligible)
    }

    fn pull_request_target(source_anchor_id: RetrievalAnchorId) -> GitTopologyAnchorTargetV1 {
        GitTopologyAnchorTargetV1::PullRequestSnapshot(PullRequestSnapshotAnchorRefV1 {
            provider: ProviderId::new("provider.github").unwrap(),
            project_id: ProjectId::new("project.fixture").unwrap(),
            repository_id: tracedecay_domain::RepositoryId::new("repository.fixture").unwrap(),
            worktree_id: WorktreeId::new("worktree.fixture").unwrap(),
            pull_request_id: tracedecay_domain::GitHubPullRequestIdV1::new("pr.42").unwrap(),
            base_commit_id: CommitId::new("commit.base").unwrap(),
            head_commit_id: CommitId::new("commit.head").unwrap(),
            merge_base_commit_id: CommitId::new("commit.merge-base").unwrap(),
            source_anchor_id: source_anchor_id.clone(),
            snapshot_digest: ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
            sources: vec![OrderedGitTopologySourceV1 {
                source_ordinal: 0,
                role: GitTopologySourceRoleV1::PullRequestObservation,
                anchor_id: source_anchor_id,
            }],
        })
    }

    fn topology_record(
        target: GitTopologyAnchorTargetV1,
        source: &RetrievalAnchorRecordV2,
    ) -> RetrievalAnchorRecordV2 {
        RetrievalAnchorRecordV2::new(RetrievalAnchorRecordV2Parts {
            source_generation: AnchorSourceGenerationV2::GitTopology(target.generation()),
            target: RetrievalAnchorTargetV2::GitTopology(Box::new(target)),
            owner: ObservationScopeV1::Project {
                project_id: ProjectId::new("project.fixture").unwrap(),
            },
            aliases: vec![],
            occurred_at: None,
            ingested_at: UtcMicros(2),
            evidence_class: EvidenceClass::Observed,
            projection_generation: ProjectionGenerationId::new("projection.fixture.v1").unwrap(),
            projection_watermark: watermark(&[("observation.projection", 1)]),
            coverage: CoverageReportV1::default(),
            source_observations: vec![],
            source_anchors: vec![
                AnchorLineageRefV2::new(
                    AnchorProvenanceRelationV2::DerivedFrom,
                    source.anchor_id().clone(),
                    source.owner().clone(),
                )
                .unwrap(),
            ],
            authorization: authorization(),
            payload_access: PayloadAccessState::Eligible,
            retention_class: RetentionClass::new("retention.fixture").unwrap(),
            durability: AnchorDurabilityClass::DurableEvidence,
        })
        .unwrap()
    }

    #[test]
    fn every_payload_access_maps_to_its_typed_state_with_coverage() {
        let exact = watermark(&[("observation.projection", 1)]);
        let cases = [
            (
                PayloadAccessState::Eligible,
                AnchorResolutionStateV2::Current,
            ),
            (
                PayloadAccessState::Redacted,
                AnchorResolutionStateV2::Redacted,
            ),
            (
                PayloadAccessState::Quarantined,
                AnchorResolutionStateV2::Redacted,
            ),
            (
                PayloadAccessState::RetentionExpired,
                AnchorResolutionStateV2::Expired,
            ),
            (
                PayloadAccessState::Deleted,
                AnchorResolutionStateV2::Deleted,
            ),
            (
                PayloadAccessState::Unavailable,
                AnchorResolutionStateV2::Unavailable,
            ),
            (
                PayloadAccessState::Ambiguous,
                AnchorResolutionStateV2::Ambiguous,
            ),
        ];
        for (access, expected) in cases {
            let report = report_for(access, exact.clone());
            assert_eq!(report.state(), expected, "{access:?}");
            assert!(report.record().is_some(), "{access:?}");
            assert_eq!(
                report.resolution().coverage(),
                report.record().unwrap().coverage()
            );
            let wire = serde_json::to_value(report.resolution()).unwrap();
            let object = wire.as_object().unwrap();
            assert!(!object.contains_key("payload"), "{access:?}");
            assert!(!object.contains_key("query"), "{access:?}");
            assert!(!object.contains_key("source_locator"), "{access:?}");
        }
    }

    #[test]
    fn eligible_record_reports_watermark_drift() {
        let report = report_for(
            PayloadAccessState::Eligible,
            watermark(&[("observation.projection", 2)]),
        );
        assert_eq!(
            report.state(),
            AnchorResolutionStateV2::Drifted {
                drift: WatermarkDriftV1::ObservedAhead
            }
        );
        assert_eq!(
            report.resolution().watermark().frozen,
            watermark(&[("observation.projection", 1)])
        );
        assert_eq!(
            report.resolution().watermark().observed,
            watermark(&[("observation.projection", 2)])
        );
    }

    #[test]
    fn unresolved_states_carry_caller_authorization_and_no_record() {
        for (observed, expected) in [
            (
                ObservedEvidenceAnchorResolution::Unavailable,
                AnchorResolutionStateV2::Unavailable,
            ),
            (
                ObservedEvidenceAnchorResolution::Ambiguous,
                AnchorResolutionStateV2::Ambiguous,
            ),
        ] {
            let report = EvidenceAnchorResolutionReport::from_observation(
                RetrievalAnchorId::new("retrieval.fixture").unwrap(),
                observed,
                authorization(),
            )
            .unwrap();
            assert_eq!(report.state(), expected);
            assert!(report.record().is_none());
            assert_eq!(report.resolution().authorization(), &authorization());
            assert_eq!(
                report.resolution().watermark().drift,
                WatermarkDriftV1::Exact
            );
            ManifestDigest::new(report.resolution().resolved_record_digest().as_str()).unwrap();
        }
    }

    #[test]
    fn mismatched_record_identity_fails_closed() {
        let record = record_with_access(PayloadAccessState::Eligible);
        let other = RetrievalAnchorId::new("retrieval.other").unwrap();
        assert_ne!(record.anchor_id(), &other);
        assert!(
            EvidenceAnchorResolutionReport::from_observation(
                other,
                ObservedEvidenceAnchorResolution::Resolved {
                    record: Box::new(record),
                    observed_watermark: VectorWatermark::default(),
                },
                authorization(),
            )
            .is_err()
        );
    }

    #[test]
    fn topology_drilldown_preserves_sources_and_reports_stale_or_retargeted() {
        let source = project_observation_record('c');
        let anchored_target = pull_request_target(source.anchor_id().clone());
        let root_record = topology_record(anchored_target.clone(), &source);
        let root = EvidenceAnchorResolutionReport::from_observation(
            root_record.anchor_id().clone(),
            ObservedEvidenceAnchorResolution::Resolved {
                record: Box::new(root_record.clone()),
                observed_watermark: watermark(&[("observation.projection", 1)]),
            },
            authorization(),
        )
        .unwrap();
        let source_report = EvidenceAnchorResolutionReport::from_observation(
            source.anchor_id().clone(),
            ObservedEvidenceAnchorResolution::Resolved {
                record: Box::new(source.clone()),
                observed_watermark: watermark(&[("observation.projection", 1)]),
            },
            authorization(),
        )
        .unwrap();
        let mut observed_target = anchored_target.clone();
        let GitTopologyAnchorTargetV1::PullRequestSnapshot(observed_pull_request) =
            &mut observed_target
        else {
            unreachable!()
        };
        observed_pull_request.head_commit_id = CommitId::new("commit.retargeted").unwrap();
        observed_pull_request.snapshot_digest =
            ManifestDigest::new(format!("sha256:{}", "d".repeat(64))).unwrap();

        let retargeted = AuthorizedGitTopologyDrilldownV1::new(
            root,
            vec![source_report],
            Some(&observed_target),
        )
        .unwrap();
        assert!(matches!(
            retargeted.state(),
            GitTopologyResolutionStateV1::Retargeted { .. }
        ));
        assert_eq!(retargeted.sources()[0].anchor_id(), source.anchor_id());

        let disposed_root = EvidenceAnchorResolutionReport::from_observation(
            root_record.anchor_id().clone(),
            ObservedEvidenceAnchorResolution::Resolved {
                record: Box::new(root_record.clone()),
                observed_watermark: watermark(&[("observation.projection", 1)]),
            },
            authorization(),
        )
        .unwrap();
        let disposed_source_record =
            project_observation_record_with_access('c', PayloadAccessState::Redacted);
        let disposed_source = EvidenceAnchorResolutionReport::from_observation(
            disposed_source_record.anchor_id().clone(),
            ObservedEvidenceAnchorResolution::Resolved {
                record: Box::new(disposed_source_record.clone()),
                observed_watermark: watermark(&[("observation.projection", 1)]),
            },
            authorization(),
        )
        .unwrap();
        let disposed =
            AuthorizedGitTopologyDrilldownV1::new(disposed_root, vec![disposed_source], None)
                .unwrap();
        assert!(matches!(
            disposed.state(),
            GitTopologyResolutionStateV1::SourceDisposition {
                source_anchor_id,
                state: AnchorResolutionStateV2::Redacted,
            } if source_anchor_id == disposed_source_record.anchor_id()
        ));

        let stale_root = EvidenceAnchorResolutionReport::from_observation(
            root_record.anchor_id().clone(),
            ObservedEvidenceAnchorResolution::Resolved {
                record: Box::new(root_record),
                observed_watermark: watermark(&[("observation.projection", 2)]),
            },
            authorization(),
        )
        .unwrap();
        let stale_source = EvidenceAnchorResolutionReport::from_observation(
            source.anchor_id().clone(),
            ObservedEvidenceAnchorResolution::Resolved {
                record: Box::new(source),
                observed_watermark: watermark(&[("observation.projection", 1)]),
            },
            authorization(),
        )
        .unwrap();
        let stale =
            AuthorizedGitTopologyDrilldownV1::new(stale_root, vec![stale_source], None).unwrap();
        assert!(matches!(
            stale.state(),
            GitTopologyResolutionStateV1::Stale { .. }
        ));
    }
}
