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

use std::future::Future;

use serde::Serialize;
use tracedecay_domain::{
    AnchorResolutionStateV2, AuthorizedAnchorResolutionV2, CoverageReportV1, DomainError,
    FactOwnerV1, FrozenWatermarkResolutionV1, PayloadAccessState, ResolutionAuthorizationV1,
    RetrievalAnchorId, RetrievalAnchorRecordV2, VectorWatermark, canonical_sha256,
};
use tracedecay_store::ObservedEvidenceAnchorResolution;

use crate::memory::EvidenceAnchorResolutionError;

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
        CanonicalObservationIdV1, CapabilityId, EvidenceClass, ManifestDigest, ObservationScopeV1,
        PrivacyDomainBoundLocatorDigest, PrivacyDomainId, ProjectionGenerationId, RetentionClass,
        RetrievalAnchorRecordV2Parts, RetrievalAnchorTargetV2, ScopeResolutionId, ShardId,
        UtcMicros, WatermarkDriftV1,
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
}
