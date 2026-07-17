use std::cmp::Ordering;

use serde::{Deserialize, Deserializer, Serialize};

use super::coverage::CoverageReportV1;
use super::error::DomainError;
use super::id::{
    AccessPolicyDigest, CapabilityId, ManifestDigest, PrivacyDomainId, RetrievalAnchorId,
    ScopeResolutionId,
};
use super::retrieval::{PayloadAccessState, PrivacyDomainBoundLocatorDigest};
use super::subjects::CatalogSnapshotRefV1;
use super::watermark::VectorWatermark;

/// Deterministic relationship between an observed store state and the state
/// frozen into a retrieval anchor.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WatermarkDriftV1 {
    Exact,
    ObservedAhead,
    ObservedBehind,
    Concurrent,
}

impl WatermarkDriftV1 {
    pub fn classify(frozen: &VectorWatermark, observed: &VectorWatermark) -> Self {
        match observed.partial_cmp_components(frozen) {
            Some(Ordering::Equal) => Self::Exact,
            Some(Ordering::Greater) => Self::ObservedAhead,
            Some(Ordering::Less) => Self::ObservedBehind,
            None => Self::Concurrent,
        }
    }
}

/// Pure resolution record that preserves both the requested snapshot and the
/// store state seen by the resolver. The drift value is validated rather than
/// trusted from the wire.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrozenWatermarkResolutionV1 {
    pub frozen: VectorWatermark,
    pub observed: VectorWatermark,
    pub drift: WatermarkDriftV1,
}

impl FrozenWatermarkResolutionV1 {
    pub fn new(frozen: VectorWatermark, observed: VectorWatermark) -> Self {
        let drift = WatermarkDriftV1::classify(&frozen, &observed);
        Self {
            frozen,
            observed,
            drift,
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if self.drift != WatermarkDriftV1::classify(&self.frozen, &self.observed) {
            return Err(DomainError::SnapshotMismatch {
                field: "frozen resolution drift",
            });
        }
        Ok(())
    }
}

/// Safe metadata proving which authorization decision bounded a resolution.
/// It deliberately contains no source locator, query text, payload, or secret.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResolutionAuthorizationV1 {
    pub resolved_scope_id: ScopeResolutionId,
    pub privacy_domain_id: PrivacyDomainId,
    pub access_policy_digest: AccessPolicyDigest,
    pub capability_id: CapabilityId,
    pub canonical_request_digest: PrivacyDomainBoundLocatorDigest,
}

impl ResolutionAuthorizationV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.resolved_scope_id.validate()?;
        self.privacy_domain_id.validate()?;
        self.access_policy_digest.validate()?;
        self.capability_id.validate()?;
        self.canonical_request_digest.validate()
    }
}

/// Stable, payload-free result metadata emitted after resolving an immutable
/// retrieval anchor at its frozen watermark.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedAnchorResolutionV1 {
    pub anchor_id: RetrievalAnchorId,
    pub catalog_snapshot: CatalogSnapshotRefV1,
    pub authorization: ResolutionAuthorizationV1,
    pub watermark: FrozenWatermarkResolutionV1,
    pub payload_access: PayloadAccessState,
    pub resolved_record_digest: ManifestDigest,
}

impl AuthorizedAnchorResolutionV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        self.anchor_id.validate()?;
        self.catalog_snapshot.validate()?;
        self.authorization.validate()?;
        self.watermark.validate()?;
        self.resolved_record_digest.validate()
    }
}

/// Outcome of resolving a V2 anchor. This describes identity resolution and
/// freshness, while [`PayloadAccessState`] independently describes whether the
/// retained payload may be accessed.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "details",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum AnchorResolutionStateV2 {
    Current,
    Drifted { drift: WatermarkDriftV1 },
    Redacted,
    Expired,
    Deleted,
    Unavailable,
    Ambiguous,
}

impl AnchorResolutionStateV2 {
    /// Classify the resolution state from the payload access declared by the
    /// retained record (or the store's binding signal) and the validated
    /// watermark drift. The result always satisfies `validate`: access states
    /// win over freshness, so a redacted, expired, deleted, unavailable, or
    /// ambiguous target is never reported as current or merely drifted.
    pub fn classify(payload_access: PayloadAccessState, drift: WatermarkDriftV1) -> Self {
        match payload_access {
            PayloadAccessState::Eligible => match drift {
                WatermarkDriftV1::Exact => Self::Current,
                drift => Self::Drifted { drift },
            },
            PayloadAccessState::Redacted | PayloadAccessState::Quarantined => Self::Redacted,
            PayloadAccessState::RetentionExpired => Self::Expired,
            PayloadAccessState::Deleted => Self::Deleted,
            PayloadAccessState::Unavailable => Self::Unavailable,
            PayloadAccessState::Ambiguous => Self::Ambiguous,
        }
    }

    fn validate(
        self,
        watermark: &FrozenWatermarkResolutionV1,
        payload_access: PayloadAccessState,
    ) -> Result<(), DomainError> {
        let valid = match self {
            Self::Current => {
                watermark.drift == WatermarkDriftV1::Exact
                    && payload_access == PayloadAccessState::Eligible
            }
            Self::Drifted { drift } => {
                drift != WatermarkDriftV1::Exact
                    && drift == watermark.drift
                    && payload_access == PayloadAccessState::Eligible
            }
            Self::Redacted => matches!(
                payload_access,
                PayloadAccessState::Redacted | PayloadAccessState::Quarantined
            ),
            Self::Expired => payload_access == PayloadAccessState::RetentionExpired,
            Self::Deleted => payload_access == PayloadAccessState::Deleted,
            Self::Unavailable => payload_access == PayloadAccessState::Unavailable,
            Self::Ambiguous => payload_access == PayloadAccessState::Ambiguous,
        };
        if !valid {
            return Err(DomainError::SnapshotMismatch {
                field: "anchor resolution state",
            });
        }
        Ok(())
    }
}

/// Payload-free V2 resolution metadata with explicit coverage and a state that
/// cannot be confused with payload retention/access policy.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthorizedAnchorResolutionV2 {
    anchor_id: RetrievalAnchorId,
    authorization: ResolutionAuthorizationV1,
    watermark: FrozenWatermarkResolutionV1,
    coverage: CoverageReportV1,
    state: AnchorResolutionStateV2,
    payload_access: PayloadAccessState,
    resolved_record_digest: ManifestDigest,
}

impl AuthorizedAnchorResolutionV2 {
    pub fn new(
        anchor_id: RetrievalAnchorId,
        authorization: ResolutionAuthorizationV1,
        watermark: FrozenWatermarkResolutionV1,
        coverage: CoverageReportV1,
        state: AnchorResolutionStateV2,
        payload_access: PayloadAccessState,
        resolved_record_digest: ManifestDigest,
    ) -> Result<Self, DomainError> {
        let value = Self {
            anchor_id,
            authorization,
            watermark,
            coverage,
            state,
            payload_access,
            resolved_record_digest,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    pub fn authorization(&self) -> &ResolutionAuthorizationV1 {
        &self.authorization
    }

    pub fn watermark(&self) -> &FrozenWatermarkResolutionV1 {
        &self.watermark
    }

    pub fn coverage(&self) -> &CoverageReportV1 {
        &self.coverage
    }

    pub fn state(&self) -> AnchorResolutionStateV2 {
        self.state
    }

    pub fn payload_access(&self) -> PayloadAccessState {
        self.payload_access
    }

    pub fn resolved_record_digest(&self) -> &ManifestDigest {
        &self.resolved_record_digest
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        self.anchor_id.validate()?;
        self.authorization.validate()?;
        self.watermark.validate()?;
        self.coverage.validate()?;
        self.resolved_record_digest.validate()?;
        self.state.validate(&self.watermark, self.payload_access)
    }
}

impl<'de> Deserialize<'de> for AuthorizedAnchorResolutionV2 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            anchor_id: RetrievalAnchorId,
            authorization: ResolutionAuthorizationV1,
            watermark: FrozenWatermarkResolutionV1,
            coverage: CoverageReportV1,
            state: AnchorResolutionStateV2,
            payload_access: PayloadAccessState,
            resolved_record_digest: ManifestDigest,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.anchor_id,
            wire.authorization,
            wire.watermark,
            wire.coverage,
            wire.state,
            wire.payload_access,
            wire.resolved_record_digest,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::*;
    use crate::research::ShardId;

    const SHA256_FIXTURE: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn watermark(values: &[(&str, u64)]) -> VectorWatermark {
        VectorWatermark {
            components: values
                .iter()
                .map(|(shard, sequence)| (ShardId::new(*shard).unwrap(), *sequence))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn classifies_all_vector_watermark_relationships_deterministically() {
        let frozen = watermark(&[("a", 3), ("b", 5)]);

        assert_eq!(
            WatermarkDriftV1::classify(&frozen, &frozen),
            WatermarkDriftV1::Exact
        );
        assert_eq!(
            WatermarkDriftV1::classify(&frozen, &watermark(&[("a", 4), ("b", 5)])),
            WatermarkDriftV1::ObservedAhead
        );
        assert_eq!(
            WatermarkDriftV1::classify(&frozen, &watermark(&[("a", 3), ("b", 4)])),
            WatermarkDriftV1::ObservedBehind
        );
        assert_eq!(
            WatermarkDriftV1::classify(&frozen, &watermark(&[("a", 4), ("b", 4)])),
            WatermarkDriftV1::Concurrent
        );
    }

    #[test]
    fn rejects_wire_claimed_drift_that_does_not_match_watermarks() {
        let value = json!({
            "frozen": { "components": { "a": 3 } },
            "observed": { "components": { "a": 4 } },
            "drift": "exact"
        });
        let resolution: FrozenWatermarkResolutionV1 = serde_json::from_value(value).unwrap();

        assert_eq!(
            resolution.validate(),
            Err(DomainError::SnapshotMismatch {
                field: "frozen resolution drift"
            })
        );
    }

    #[test]
    fn resolution_metadata_rejects_unknown_wire_fields() {
        let value = json!({
            "frozen": { "components": {} },
            "observed": { "components": {} },
            "drift": "exact",
            "payload": "must never be accepted"
        });

        assert!(serde_json::from_value::<FrozenWatermarkResolutionV1>(value).is_err());
    }

    #[test]
    fn authorized_resolution_is_valid_and_payload_free() {
        let resolution = AuthorizedAnchorResolutionV1 {
            anchor_id: RetrievalAnchorId::new("anchor.fixture").unwrap(),
            catalog_snapshot: CatalogSnapshotRefV1 {
                generation: crate::research::CatalogGenerationId::new("catalog.fixture").unwrap(),
                digest: ManifestDigest::new(SHA256_FIXTURE).unwrap(),
            },
            authorization: ResolutionAuthorizationV1 {
                resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
                privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
                access_policy_digest: AccessPolicyDigest::new(SHA256_FIXTURE).unwrap(),
                capability_id: CapabilityId::new("capability.fixture").unwrap(),
                canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(SHA256_FIXTURE)
                    .unwrap(),
            },
            watermark: FrozenWatermarkResolutionV1::new(
                watermark(&[("a", 3)]),
                watermark(&[("a", 4)]),
            ),
            payload_access: PayloadAccessState::Eligible,
            resolved_record_digest: ManifestDigest::new(SHA256_FIXTURE).unwrap(),
        };

        resolution.validate().unwrap();
        let wire = serde_json::to_value(resolution).unwrap();
        let object = wire.as_object().unwrap();
        assert!(!object.contains_key("payload"));
        assert!(!object.contains_key("query"));
        assert!(!object.contains_key("source_locator"));
    }

    fn authorization() -> ResolutionAuthorizationV1 {
        ResolutionAuthorizationV1 {
            resolved_scope_id: ScopeResolutionId::new("scope.fixture").unwrap(),
            privacy_domain_id: PrivacyDomainId::new("privacy.fixture").unwrap(),
            access_policy_digest: AccessPolicyDigest::new(SHA256_FIXTURE).unwrap(),
            capability_id: CapabilityId::new("capability.fixture").unwrap(),
            canonical_request_digest: PrivacyDomainBoundLocatorDigest::new(SHA256_FIXTURE).unwrap(),
        }
    }

    fn v2_resolution(
        state: AnchorResolutionStateV2,
        payload_access: PayloadAccessState,
    ) -> AuthorizedAnchorResolutionV2 {
        AuthorizedAnchorResolutionV2::new(
            RetrievalAnchorId::new("anchor.fixture").unwrap(),
            authorization(),
            FrozenWatermarkResolutionV1::new(watermark(&[("a", 3)]), watermark(&[("a", 3)])),
            CoverageReportV1::default(),
            state,
            payload_access,
            ManifestDigest::new(SHA256_FIXTURE).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn unavailable_and_deleted_v2_resolutions_are_payload_free() {
        for resolution in [
            v2_resolution(
                AnchorResolutionStateV2::Unavailable,
                PayloadAccessState::Unavailable,
            ),
            v2_resolution(
                AnchorResolutionStateV2::Deleted,
                PayloadAccessState::Deleted,
            ),
        ] {
            let wire = serde_json::to_value(resolution).unwrap();
            let object = wire.as_object().unwrap();
            assert!(!object.contains_key("payload"));
            assert!(!object.contains_key("query"));
            assert!(!object.contains_key("path"));
            assert!(!object.contains_key("source_locator"));
        }
    }

    #[test]
    fn v2_resolution_rejects_state_access_tampering() {
        let mut wire = serde_json::to_value(v2_resolution(
            AnchorResolutionStateV2::Deleted,
            PayloadAccessState::Deleted,
        ))
        .unwrap();
        wire["payload_access"] = json!("eligible");

        assert!(serde_json::from_value::<AuthorizedAnchorResolutionV2>(wire).is_err());
    }

    #[test]
    fn classified_states_always_validate_against_their_inputs() {
        let drifts = [
            WatermarkDriftV1::Exact,
            WatermarkDriftV1::ObservedAhead,
            WatermarkDriftV1::ObservedBehind,
            WatermarkDriftV1::Concurrent,
        ];
        let accesses = [
            PayloadAccessState::Eligible,
            PayloadAccessState::Redacted,
            PayloadAccessState::Quarantined,
            PayloadAccessState::RetentionExpired,
            PayloadAccessState::Deleted,
            PayloadAccessState::Unavailable,
            PayloadAccessState::Ambiguous,
        ];
        for access in accesses {
            for drift in drifts {
                let state = AnchorResolutionStateV2::classify(access, drift);
                let (frozen, observed) = match drift {
                    WatermarkDriftV1::Exact => (watermark(&[("a", 3)]), watermark(&[("a", 3)])),
                    WatermarkDriftV1::ObservedAhead => {
                        (watermark(&[("a", 3)]), watermark(&[("a", 4)]))
                    }
                    WatermarkDriftV1::ObservedBehind => {
                        (watermark(&[("a", 3)]), watermark(&[("a", 2)]))
                    }
                    WatermarkDriftV1::Concurrent => (
                        watermark(&[("a", 3), ("b", 3)]),
                        watermark(&[("a", 4), ("b", 2)]),
                    ),
                };
                let resolution = AuthorizedAnchorResolutionV2::new(
                    RetrievalAnchorId::new("anchor.fixture").unwrap(),
                    authorization(),
                    FrozenWatermarkResolutionV1::new(frozen, observed),
                    CoverageReportV1::default(),
                    state,
                    access,
                    ManifestDigest::new(SHA256_FIXTURE).unwrap(),
                )
                .unwrap();
                assert_eq!(resolution.state(), state);
            }
        }
        assert_eq!(
            AnchorResolutionStateV2::classify(
                PayloadAccessState::Eligible,
                WatermarkDriftV1::Exact
            ),
            AnchorResolutionStateV2::Current
        );
        assert_eq!(
            AnchorResolutionStateV2::classify(
                PayloadAccessState::Eligible,
                WatermarkDriftV1::ObservedAhead
            ),
            AnchorResolutionStateV2::Drifted {
                drift: WatermarkDriftV1::ObservedAhead
            }
        );
        assert_eq!(
            AnchorResolutionStateV2::classify(
                PayloadAccessState::Quarantined,
                WatermarkDriftV1::Exact
            ),
            AnchorResolutionStateV2::Redacted
        );
    }
}
