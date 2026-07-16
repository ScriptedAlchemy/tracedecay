use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
    fn validate(&self, fact_id: &FactId) -> Result<(), DomainError> {
        match self {
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
                validate_evidence_ids(evidence_ids)
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
                        if related == fact_id {
                            return Err(DomainError::SelfSupersession);
                        }
                    }
                    FactCurationActionV1::Retained | FactCurationActionV1::Forgotten => {}
                }
                validate_evidence_ids(evidence_ids)
            }
            Self::PayloadAccessChanged { previous, current } => {
                if previous == current {
                    return Err(DomainError::NonCanonical {
                        field: "fact payload access transition",
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
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
        kind.validate(&fact_id)?;
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

fn validate_evidence_ids(evidence_ids: &[FactEvidenceId]) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    for evidence_id in evidence_ids {
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
}
