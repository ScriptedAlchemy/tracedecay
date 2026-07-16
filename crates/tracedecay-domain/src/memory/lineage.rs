use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use super::{FactIdentityMaterialV1, FactIdentitySourceV1, FactOwnerV1, derive_memory_id};
use crate::research::{
    ActorId, Confidence, DomainError, FactAssertionId, FactEventId, FactEvidenceId, FactId,
    PayloadAccessState, SourceStoreId, UtcMicros,
};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LegacyHistoryCoverageV1 {
    Complete,
    Unknown,
}

const MAX_LINEAGE_EVIDENCE_REFS: usize = 256;

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LegacyFactMappingV1 {
    owner: FactOwnerV1,
    source_store_id: SourceStoreId,
    legacy_fact_id: i64,
    fact_id: FactId,
    history_coverage: LegacyHistoryCoverageV1,
    migrated_at: UtcMicros,
}

impl LegacyFactMappingV1 {
    pub fn new(
        owner: FactOwnerV1,
        source_store_id: SourceStoreId,
        legacy_fact_id: i64,
        fact_id: FactId,
        history_coverage: LegacyHistoryCoverageV1,
        migrated_at: UtcMicros,
    ) -> Result<Self, DomainError> {
        owner.validate()?;
        source_store_id.validate()?;
        fact_id.validate()?;
        fact_id.validate_owner(&owner)?;
        if legacy_fact_id <= 0 {
            return Err(DomainError::NonCanonical {
                field: "legacy fact id",
            });
        }
        let expected_fact_id = FactId::derive(&FactIdentityMaterialV1::new(
            owner.clone(),
            FactIdentitySourceV1::Legacy {
                source_store_id: source_store_id.clone(),
                legacy_fact_id,
            },
        )?)?;
        if fact_id != expected_fact_id {
            return Err(DomainError::UnknownReference {
                field: "legacy mapping fact identity",
            });
        }
        Ok(Self {
            owner,
            source_store_id,
            legacy_fact_id,
            fact_id,
            history_coverage,
            migrated_at,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn source_store_id(&self) -> &SourceStoreId {
        &self.source_store_id
    }

    pub fn legacy_fact_id(&self) -> i64 {
        self.legacy_fact_id
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn history_coverage(&self) -> LegacyHistoryCoverageV1 {
        self.history_coverage
    }

    pub fn migrated_at(&self) -> UtcMicros {
        self.migrated_at
    }
}

impl<'de> Deserialize<'de> for LegacyFactMappingV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            owner: FactOwnerV1,
            source_store_id: SourceStoreId,
            legacy_fact_id: i64,
            fact_id: FactId,
            history_coverage: LegacyHistoryCoverageV1,
            migrated_at: UtcMicros,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.owner,
            wire.source_store_id,
            wire.legacy_fact_id,
            wire.fact_id,
            wire.history_coverage,
            wire.migrated_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactCurationActionV1 {
    Retained,
    ContradictedBy { fact_id: FactId },
    SupersededBy { fact_id: FactId },
    MergedInto { fact_id: FactId },
    Forgotten,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactLineageEventKindV1 {
    AssertionRecorded {
        assertion_id: FactAssertionId,
    },
    TrustChanged {
        previous: Confidence,
        current: Confidence,
        evidence_ids: Vec<FactEvidenceId>,
    },
    Curated {
        action: FactCurationActionV1,
        evidence_ids: Vec<FactEvidenceId>,
    },
    PayloadAccessChanged {
        previous: PayloadAccessState,
        current: PayloadAccessState,
    },
    LegacyImported {
        mapping: LegacyFactMappingV1,
    },
}

impl FactLineageEventKindV1 {
    fn canonicalized(mut self, fact_id: &FactId, owner: &FactOwnerV1) -> Result<Self, DomainError> {
        match &mut self {
            Self::AssertionRecorded { assertion_id } => assertion_id.validate(),
            Self::TrustChanged {
                previous,
                current,
                evidence_ids,
            } => {
                if previous == current {
                    return Err(DomainError::NonCanonical {
                        field: "fact trust transition",
                    });
                }
                canonicalize_evidence_ids(evidence_ids)
            }
            Self::Curated {
                action,
                evidence_ids,
            } => {
                match action {
                    FactCurationActionV1::ContradictedBy { fact_id: related }
                    | FactCurationActionV1::SupersededBy { fact_id: related }
                    | FactCurationActionV1::MergedInto { fact_id: related } => {
                        related.validate()?;
                        related.validate_owner(owner)?;
                        if related == fact_id {
                            return Err(DomainError::SelfSupersession);
                        }
                    }
                    FactCurationActionV1::Retained | FactCurationActionV1::Forgotten => {}
                }
                canonicalize_evidence_ids(evidence_ids)
            }
            Self::PayloadAccessChanged { previous, current } => {
                if previous == current {
                    return Err(DomainError::NonCanonical {
                        field: "fact payload access transition",
                    });
                }
                if *previous == PayloadAccessState::Deleted {
                    return Err(DomainError::NonCanonical {
                        field: "terminal fact payload deletion",
                    });
                }
                Ok(())
            }
            Self::LegacyImported { mapping } => {
                if mapping.fact_id() != fact_id {
                    return Err(DomainError::UnknownReference {
                        field: "legacy mapping fact",
                    });
                }
                if mapping.owner() != owner {
                    return Err(DomainError::UnknownReference {
                        field: "legacy mapping owner",
                    });
                }
                Ok(())
            }
        }?;
        Ok(self)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactLineageEventV1 {
    event_id: FactEventId,
    fact_id: FactId,
    owner: FactOwnerV1,
    kind: FactLineageEventKindV1,
    occurred_at: UtcMicros,
    actor_id: Option<ActorId>,
}

#[derive(Serialize)]
struct FactEventIdentityMaterial<'a> {
    fact_id: &'a FactId,
    owner: &'a FactOwnerV1,
    kind: &'a FactLineageEventKindV1,
    occurred_at: UtcMicros,
    actor_id: Option<&'a ActorId>,
}

impl FactLineageEventV1 {
    pub fn new(
        fact_id: FactId,
        owner: FactOwnerV1,
        kind: FactLineageEventKindV1,
        occurred_at: UtcMicros,
        actor_id: Option<ActorId>,
    ) -> Result<Self, DomainError> {
        fact_id.validate()?;
        owner.validate()?;
        fact_id.validate_owner(&owner)?;
        let kind = kind.canonicalized(&fact_id, &owner)?;
        if let Some(actor_id) = &actor_id {
            actor_id.validate()?;
        }
        let event_id = FactEventId::new(derive_memory_id(
            "fact-event.v1",
            &FactEventIdentityMaterial {
                fact_id: &fact_id,
                owner: &owner,
                kind: &kind,
                occurred_at,
                actor_id: actor_id.as_ref(),
            },
        )?)?;
        Ok(Self {
            event_id,
            fact_id,
            owner,
            kind,
            occurred_at,
            actor_id,
        })
    }

    pub fn event_id(&self) -> &FactEventId {
        &self.event_id
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn kind(&self) -> &FactLineageEventKindV1 {
        &self.kind
    }

    pub fn occurred_at(&self) -> UtcMicros {
        self.occurred_at
    }

    pub fn actor_id(&self) -> Option<&ActorId> {
        self.actor_id.as_ref()
    }
}

impl<'de> Deserialize<'de> for FactLineageEventV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            event_id: FactEventId,
            fact_id: FactId,
            owner: FactOwnerV1,
            kind: FactLineageEventKindV1,
            occurred_at: UtcMicros,
            actor_id: Option<ActorId>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let claimed_id = wire.event_id;
        let event = Self::new(
            wire.fact_id,
            wire.owner,
            wire.kind,
            wire.occurred_at,
            wire.actor_id,
        )
        .map_err(serde::de::Error::custom)?;
        if claimed_id != event.event_id {
            return Err(serde::de::Error::custom(DomainError::DigestMismatch));
        }
        Ok(event)
    }
}

fn canonicalize_evidence_ids(evidence_ids: &mut [FactEvidenceId]) -> Result<(), DomainError> {
    if evidence_ids.len() > MAX_LINEAGE_EVIDENCE_REFS {
        return Err(DomainError::NonCanonical {
            field: "fact event evidence",
        });
    }
    evidence_ids.sort_unstable();
    let mut seen = BTreeSet::new();
    for evidence_id in evidence_ids.iter() {
        evidence_id.validate()?;
        if !seen.insert(evidence_id) {
            return Err(DomainError::DuplicateId {
                field: "fact event evidence",
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::ProvenanceId;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String, Error = DomainError>,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn fact_id(operation: &str) -> FactId {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                FactOwnerV1::Profile,
                FactIdentitySourceV1::Application {
                    operation_id: id::<ProvenanceId>(operation),
                },
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn lineage_event_identity_is_deterministic() {
        let fact_id = fact_id("operation.fixture");
        let first = FactLineageEventV1::new(
            fact_id.clone(),
            FactOwnerV1::Profile,
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(20),
            None,
        )
        .unwrap();
        let replay = FactLineageEventV1::new(
            fact_id,
            FactOwnerV1::Profile,
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(20),
            None,
        )
        .unwrap();
        assert_eq!(first.event_id(), replay.event_id());
    }

    #[test]
    fn curation_rejects_self_supersession() {
        let fact_id = fact_id("operation.fixture");
        assert!(
            FactLineageEventV1::new(
                fact_id.clone(),
                FactOwnerV1::Profile,
                FactLineageEventKindV1::Curated {
                    action: FactCurationActionV1::SupersededBy { fact_id },
                    evidence_ids: vec![],
                },
                UtcMicros(20),
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_mapping_preserves_explicit_unknown_history() {
        let source_store_id: SourceStoreId = id("store.v1");
        let fact_id = FactId::derive(
            &FactIdentityMaterialV1::new(
                FactOwnerV1::Profile,
                FactIdentitySourceV1::Legacy {
                    source_store_id: source_store_id.clone(),
                    legacy_fact_id: 7,
                },
            )
            .unwrap(),
        )
        .unwrap();
        let mapping = LegacyFactMappingV1::new(
            FactOwnerV1::Profile,
            source_store_id,
            7,
            fact_id.clone(),
            LegacyHistoryCoverageV1::Unknown,
            UtcMicros(30),
        )
        .unwrap();
        let mut mapping_wire = serde_json::to_value(&mapping).unwrap();
        mapping_wire["fact_id"] = serde_json::json!("fact.v1.forged");
        assert!(serde_json::from_value::<LegacyFactMappingV1>(mapping_wire).is_err());
        let event = FactLineageEventV1::new(
            fact_id,
            FactOwnerV1::Profile,
            FactLineageEventKindV1::LegacyImported { mapping },
            UtcMicros(30),
            None,
        )
        .unwrap();
        assert!(matches!(
            event.kind(),
            FactLineageEventKindV1::LegacyImported { mapping }
                if mapping.history_coverage() == LegacyHistoryCoverageV1::Unknown
        ));
    }

    #[test]
    fn lineage_wire_rejects_tampered_identity() {
        let event = FactLineageEventV1::new(
            fact_id("operation.wire"),
            FactOwnerV1::Profile,
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Eligible,
                current: PayloadAccessState::Deleted,
            },
            UtcMicros(20),
            None,
        )
        .unwrap();
        let mut wire = serde_json::to_value(event).unwrap();
        wire["event_id"] = serde_json::json!("fact-event.v1.forged");

        assert!(serde_json::from_value::<FactLineageEventV1>(wire).is_err());
    }

    #[test]
    fn deletion_is_terminal() {
        let result = FactLineageEventV1::new(
            fact_id("operation.deleted"),
            FactOwnerV1::Profile,
            FactLineageEventKindV1::PayloadAccessChanged {
                previous: PayloadAccessState::Deleted,
                current: PayloadAccessState::Eligible,
            },
            UtcMicros(21),
            None,
        );

        assert!(result.is_err());
    }

    #[test]
    fn lineage_evidence_order_is_canonical() {
        let fact_id = fact_id("operation.evidence-order");
        let first = FactLineageEventV1::new(
            fact_id.clone(),
            FactOwnerV1::Profile,
            FactLineageEventKindV1::TrustChanged {
                previous: Confidence::new(0.4).unwrap(),
                current: Confidence::new(0.8).unwrap(),
                evidence_ids: vec![id("evidence.b"), id("evidence.a")],
            },
            UtcMicros(22),
            None,
        )
        .unwrap();
        let second = FactLineageEventV1::new(
            fact_id,
            FactOwnerV1::Profile,
            FactLineageEventKindV1::TrustChanged {
                previous: Confidence::new(0.4).unwrap(),
                current: Confidence::new(0.8).unwrap(),
                evidence_ids: vec![id("evidence.a"), id("evidence.b")],
            },
            UtcMicros(22),
            None,
        )
        .unwrap();

        assert_eq!(first, second);
    }
}
