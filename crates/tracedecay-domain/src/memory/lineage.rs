use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

use super::{derive_memory_id, fact::FactOwnerV1, relation::FactRelationV1};
use crate::research::{
    ActorId, Confidence, DomainError, FactAssertionId, FactEventId, FactEvidenceId, FactId,
    PayloadAccessState, UtcMicros,
};

const MAX_LINEAGE_EVIDENCE_REFS: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactCurationActionV1 {
    Retained,
    TagsNormalized {
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
    },
    ContradictedBy {
        fact_id: FactId,
    },
    SupersededBy {
        fact_id: FactId,
    },
    MergedInto {
        fact_id: FactId,
    },
    Linked {
        relation: FactRelationV1,
    },
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
                    FactCurationActionV1::Linked { relation } => {
                        relation.validate()?;
                        if relation.owner() != owner {
                            return Err(DomainError::UnknownReference {
                                field: "fact relation owner",
                            });
                        }
                        if relation.source_fact_id() != fact_id {
                            return Err(DomainError::UnknownReference {
                                field: "fact relation source",
                            });
                        }
                    }
                    FactCurationActionV1::TagsNormalized {
                        evidence_fact_ids,
                        confidence,
                    } => {
                        canonicalize_evidence_fact_ids(evidence_fact_ids, owner)?;
                        Confidence::new(confidence.as_f64())?;
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

fn canonicalize_evidence_fact_ids(
    evidence_fact_ids: &mut [FactId],
    owner: &FactOwnerV1,
) -> Result<(), DomainError> {
    if evidence_fact_ids.is_empty() || evidence_fact_ids.len() > MAX_LINEAGE_EVIDENCE_REFS {
        return Err(DomainError::NonCanonical {
            field: "normalized tag evidence facts",
        });
    }
    evidence_fact_ids.sort_unstable();
    let mut seen = BTreeSet::new();
    for fact_id in evidence_fact_ids.iter() {
        fact_id.validate()?;
        fact_id.validate_owner(owner)?;
        if !seen.insert(fact_id) {
            return Err(DomainError::DuplicateId {
                field: "normalized tag evidence facts",
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::relation::{
        FactRelationKindV1, fact_id_for, new_relation, relation_evidence,
    };
    use super::*;

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String, Error = DomainError>,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn fact_id(operation: &str) -> FactId {
        fact_id_for(&FactOwnerV1::Profile, operation)
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
    fn normalized_tag_evidence_is_canonical_and_preserves_self_evidence() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id_for(&owner, "operation.normalize.subject");
        let first_evidence = fact_id_for(&owner, "operation.normalize.first-evidence");
        let second_evidence = fact_id_for(&owner, "operation.normalize.second-evidence");
        let confidence = Confidence::new(0.91).expect("normalized-tag confidence");
        let action = FactCurationActionV1::TagsNormalized {
            evidence_fact_ids: vec![second_evidence, fact_id.clone(), first_evidence],
            confidence,
        };

        let event = FactLineageEventV1::new(
            fact_id.clone(),
            owner,
            FactLineageEventKindV1::Curated {
                action,
                evidence_ids: vec![],
            },
            UtcMicros(23),
            None,
        )
        .expect("canonical normalized-tag event");
        let FactLineageEventKindV1::Curated {
            action:
                FactCurationActionV1::TagsNormalized {
                    evidence_fact_ids,
                    confidence: persisted_confidence,
                },
            evidence_ids,
        } = event.kind()
        else {
            panic!("normalized-tag action was not preserved");
        };

        assert!(evidence_ids.is_empty());
        assert_eq!(*persisted_confidence, confidence);
        assert_eq!(evidence_fact_ids.len(), 3);
        assert!(evidence_fact_ids.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(evidence_fact_ids.contains(&fact_id));
    }

    #[test]
    fn normalized_tag_evidence_rejects_duplicates_and_foreign_owners() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id_for(&owner, "operation.normalize.validation-subject");
        let evidence = fact_id_for(&owner, "operation.normalize.duplicate-evidence");
        let duplicate_action = FactCurationActionV1::TagsNormalized {
            evidence_fact_ids: vec![evidence.clone(), evidence],
            confidence: Confidence::new(0.8).expect("duplicate evidence confidence"),
        };
        assert!(matches!(
            FactLineageEventV1::new(
                fact_id.clone(),
                owner.clone(),
                FactLineageEventKindV1::Curated {
                    action: duplicate_action,
                    evidence_ids: vec![],
                },
                UtcMicros(24),
                None,
            ),
            Err(DomainError::DuplicateId { .. })
        ));

        let foreign_owner = FactOwnerV1::Project {
            project_id: id("project.normalize.foreign"),
        };
        let foreign_evidence = fact_id_for(&foreign_owner, "operation.normalize.foreign-evidence");
        let foreign_action = FactCurationActionV1::TagsNormalized {
            evidence_fact_ids: vec![foreign_evidence],
            confidence: Confidence::new(0.8).expect("foreign evidence confidence"),
        };
        assert!(
            FactLineageEventV1::new(
                fact_id,
                owner,
                FactLineageEventKindV1::Curated {
                    action: foreign_action,
                    evidence_ids: vec![],
                },
                UtcMicros(25),
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn normalized_tag_evidence_obeys_the_lineage_bound() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id_for(&owner, "operation.normalize.bound-subject");
        let evidence = (0..=MAX_LINEAGE_EVIDENCE_REFS)
            .map(|index| fact_id_for(&owner, &format!("operation.normalize.evidence.{index}")))
            .collect::<Vec<_>>();
        let action = FactCurationActionV1::TagsNormalized {
            evidence_fact_ids: evidence,
            confidence: Confidence::new(0.8).expect("over-bound evidence confidence"),
        };

        assert!(matches!(
            FactLineageEventV1::new(
                fact_id,
                owner,
                FactLineageEventKindV1::Curated {
                    action,
                    evidence_ids: vec![],
                },
                UtcMicros(26),
                None,
            ),
            Err(DomainError::NonCanonical { .. })
        ));
    }

    #[test]
    fn linked_lineage_records_every_canonical_relation_kind() {
        let owner = FactOwnerV1::Profile;
        let source_fact_id = fact_id_for(&owner, "operation.relation.source");
        let target_fact_id = fact_id_for(&owner, "operation.relation.target");
        let evidence_fact_ids = relation_evidence(&owner);
        for (kind, wire_name) in [
            (FactRelationKindV1::Supports, "supports"),
            (FactRelationKindV1::Contradicts, "contradicts"),
            (FactRelationKindV1::Supersedes, "supersedes"),
            (FactRelationKindV1::DerivedFrom, "derived_from"),
        ] {
            let relation = new_relation(
                owner.clone(),
                source_fact_id.clone(),
                target_fact_id.clone(),
                kind,
                evidence_fact_ids.clone(),
            )
            .unwrap();
            let event = FactLineageEventV1::new(
                source_fact_id.clone(),
                owner.clone(),
                FactLineageEventKindV1::Curated {
                    action: FactCurationActionV1::Linked { relation },
                    evidence_ids: vec![],
                },
                UtcMicros(23),
                None,
            )
            .unwrap();

            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(wire_name)
            );
            assert!(matches!(
                event.kind(),
                FactLineageEventKindV1::Curated {
                    action: FactCurationActionV1::Linked { relation },
                    ..
                } if relation.kind() == kind
                    && relation.target_fact_id() == &target_fact_id
                    && relation.evidence_fact_ids() == evidence_fact_ids.as_slice()
                    && relation.source_label() == "curation.fixture"
            ));
        }
    }

    #[test]
    fn linked_relation_material_is_bound_to_the_event_identity() {
        let owner = FactOwnerV1::Profile;
        let source = fact_id_for(&owner, "operation.relation.identity-source");
        let target = fact_id_for(&owner, "operation.relation.identity-target");
        let event = |kind| {
            FactLineageEventV1::new(
                source.clone(),
                owner.clone(),
                FactLineageEventKindV1::Curated {
                    action: FactCurationActionV1::Linked {
                        relation: new_relation(
                            owner.clone(),
                            source.clone(),
                            target.clone(),
                            kind,
                            relation_evidence(&owner),
                        )
                        .unwrap(),
                    },
                    evidence_ids: vec![],
                },
                UtcMicros(25),
                None,
            )
            .unwrap()
        };
        let supports = event(FactRelationKindV1::Supports);
        let contradicts = event(FactRelationKindV1::Contradicts);
        assert_ne!(supports.event_id(), contradicts.event_id());

        let mut wire = serde_json::to_value(supports).unwrap();
        wire["kind"]["action"]["relation"]["kind"] = serde_json::json!("contradicts");
        assert!(serde_json::from_value::<FactLineageEventV1>(wire).is_err());
    }

    #[test]
    fn linked_lineage_rejects_another_source_or_owner() {
        let owner = FactOwnerV1::Profile;
        let source_fact_id = fact_id_for(&owner, "operation.relation.source");
        let other_source_fact_id = fact_id_for(&owner, "operation.relation.other-source");
        let target_fact_id = fact_id_for(&owner, "operation.relation.target");
        let relation = new_relation(
            owner.clone(),
            source_fact_id,
            target_fact_id,
            FactRelationKindV1::Supersedes,
            relation_evidence(&owner),
        )
        .unwrap();
        assert!(
            FactLineageEventV1::new(
                other_source_fact_id,
                owner,
                FactLineageEventKindV1::Curated {
                    action: FactCurationActionV1::Linked { relation },
                    evidence_ids: vec![],
                },
                UtcMicros(24),
                None,
            )
            .is_err()
        );

        let event_owner = FactOwnerV1::Profile;
        let event_source = fact_id_for(&event_owner, "operation.relation.event-source");
        let relation_owner = FactOwnerV1::Project {
            project_id: id("project.relation"),
        };
        let relation = new_relation(
            relation_owner.clone(),
            fact_id_for(&relation_owner, "operation.relation.project-source"),
            fact_id_for(&relation_owner, "operation.relation.project-target"),
            FactRelationKindV1::Supersedes,
            relation_evidence(&relation_owner),
        )
        .unwrap();
        assert!(
            FactLineageEventV1::new(
                event_source,
                event_owner,
                FactLineageEventKindV1::Curated {
                    action: FactCurationActionV1::Linked { relation },
                    evidence_ids: vec![],
                },
                UtcMicros(24),
                None,
            )
            .is_err()
        );
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
