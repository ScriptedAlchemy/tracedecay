use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::fact::FactOwnerV1;
use crate::observation::{PayloadReferenceV1, SanitizationReceiptV1};
use crate::research::{Confidence, DomainError, FactId, canonical_json_bytes};

const MAX_FACT_RELATION_EVIDENCE_FACTS: usize = 256;
const MAX_FACT_RELATION_METADATA_BYTES: usize = 4 * 1024;
const MAX_FACT_RELATION_SOURCE_LABEL_BYTES: usize = 4 * 1024;

/// The finite relationship vocabulary emitted by canonical memory curation.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FactRelationKindV1 {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
}

/// The finite relationship vocabulary exposed by the verified project-memory graph.
#[derive(
    Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum ProjectMemoryGraphRelationKindV1 {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
    Mentions,
    ActiveAssertion,
    EvidenceAnchor,
}

impl FactRelationKindV1 {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::Supersedes => "supersedes",
            Self::DerivedFrom => "derived_from",
        }
    }
}

/// Receipt-bound metadata proving the exact relation provenance crossed the
/// canonical sanitizer boundary before it became durable event material.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactRelationProvenanceV1 {
    source_label: String,
    metadata: Value,
    sanitization_receipt: SanitizationReceiptV1,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct FactRelationProvenanceMaterial<'a> {
    source_label: &'a str,
    metadata: &'a Value,
}

impl FactRelationProvenanceV1 {
    pub fn new(
        source_label: String,
        metadata: Value,
        sanitization_receipt: SanitizationReceiptV1,
    ) -> Result<Self, DomainError> {
        let provenance = Self {
            source_label,
            metadata,
            sanitization_receipt,
        };
        provenance.validate()?;
        Ok(provenance)
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub fn metadata(&self) -> &Value {
        &self.metadata
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        &self.sanitization_receipt
    }

    fn validate(&self) -> Result<(), DomainError> {
        if !crate::canonical_text::is_canonical_text_within(
            &self.source_label,
            MAX_FACT_RELATION_SOURCE_LABEL_BYTES,
        ) {
            return Err(DomainError::NonCanonical {
                field: "fact relation source label",
            });
        }
        let material = FactRelationProvenanceMaterial {
            source_label: &self.source_label,
            metadata: &self.metadata,
        };
        if canonical_json_bytes(&self.metadata)?.len() > MAX_FACT_RELATION_METADATA_BYTES {
            return Err(DomainError::NonCanonical {
                field: "fact relation provenance metadata",
            });
        }
        let value = serde_json::to_value(&material).map_err(|_| DomainError::NonCanonical {
            field: "fact relation provenance",
        })?;
        if !self
            .sanitization_receipt
            .disposition()
            .permits_durable_payload()
        {
            return Err(DomainError::NonCanonical {
                field: "fact relation provenance sanitization disposition",
            });
        }
        let payload_reference =
            PayloadReferenceV1::for_payload(&value).map_err(|_| DomainError::NonCanonical {
                field: "fact relation provenance",
            })?;
        if self.sanitization_receipt.payload() != Some(&payload_reference) {
            return Err(DomainError::SnapshotMismatch {
                field: "fact relation provenance sanitization receipt",
            });
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for FactRelationProvenanceV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            source_label: String,
            metadata: Value,
            sanitization_receipt: SanitizationReceiptV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.source_label, wire.metadata, wire.sanitization_receipt)
            .map_err(serde::de::Error::custom)
    }
}

/// Immutable owner-bound relation material recorded by a lineage event.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactRelationV1 {
    owner: FactOwnerV1,
    source_fact_id: FactId,
    target_fact_id: FactId,
    kind: FactRelationKindV1,
    evidence_fact_ids: Vec<FactId>,
    confidence: Confidence,
    provenance: FactRelationProvenanceV1,
}

impl FactRelationV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        source_fact_id: FactId,
        target_fact_id: FactId,
        kind: FactRelationKindV1,
        evidence_fact_ids: Vec<FactId>,
        confidence: Confidence,
        provenance: FactRelationProvenanceV1,
    ) -> Result<Self, DomainError> {
        let relation = Self {
            owner,
            source_fact_id,
            target_fact_id,
            kind,
            evidence_fact_ids,
            confidence,
            provenance,
        };
        relation.validate()?;
        Ok(relation)
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn source_fact_id(&self) -> &FactId {
        &self.source_fact_id
    }

    pub fn target_fact_id(&self) -> &FactId {
        &self.target_fact_id
    }

    pub const fn kind(&self) -> FactRelationKindV1 {
        self.kind
    }

    pub fn evidence_fact_ids(&self) -> &[FactId] {
        &self.evidence_fact_ids
    }

    pub const fn confidence(&self) -> Confidence {
        self.confidence
    }

    pub fn source_label(&self) -> &str {
        self.provenance.source_label()
    }

    pub fn provenance(&self) -> &FactRelationProvenanceV1 {
        &self.provenance
    }

    pub(super) fn validate(&self) -> Result<(), DomainError> {
        self.owner.validate()?;
        self.source_fact_id.validate_owner(&self.owner)?;
        self.target_fact_id.validate_owner(&self.owner)?;
        if self.source_fact_id == self.target_fact_id {
            return Err(DomainError::NonCanonical {
                field: "fact relation endpoints",
            });
        }
        if self.evidence_fact_ids.is_empty() {
            return Err(DomainError::Empty {
                field: "fact relation evidence",
            });
        }
        if self.evidence_fact_ids.len() > MAX_FACT_RELATION_EVIDENCE_FACTS {
            return Err(DomainError::NonCanonical {
                field: "fact relation evidence",
            });
        }
        for evidence_fact_id in &self.evidence_fact_ids {
            evidence_fact_id.validate_owner(&self.owner)?;
        }
        for pair in self.evidence_fact_ids.windows(2) {
            if pair[0] == pair[1] {
                return Err(DomainError::DuplicateId {
                    field: "fact relation evidence",
                });
            }
            if pair[0] > pair[1] {
                return Err(DomainError::NonCanonical {
                    field: "fact relation evidence order",
                });
            }
        }
        Confidence::new(self.confidence.as_f64())?;
        self.provenance.validate()
    }
}

impl<'de> Deserialize<'de> for FactRelationV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            owner: FactOwnerV1,
            source_fact_id: FactId,
            target_fact_id: FactId,
            kind: FactRelationKindV1,
            evidence_fact_ids: Vec<FactId>,
            confidence: Confidence,
            provenance: FactRelationProvenanceV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.owner,
            wire.source_fact_id,
            wire.target_fact_id,
            wire.kind,
            wire.evidence_fact_ids,
            wire.confidence,
            wire.provenance,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
use super::{FactIdentityMaterialV1, FactIdentitySourceV1};
#[cfg(test)]
use crate::observation::{SanitizerDispositionV1, SensitivityV1};
#[cfg(test)]
use crate::research::{
    ComponentVersion, ProvenanceId, SanitizationReceiptId, SanitizationReceiptRefV1,
};

#[cfg(test)]
fn id<T>(value: &str) -> T
where
    T: TryFrom<String, Error = DomainError>,
{
    T::try_from(value.to_owned()).unwrap()
}

#[cfg(test)]
pub(in crate::memory) fn fact_id_for(owner: &FactOwnerV1, operation: &str) -> FactId {
    FactId::derive(
        &FactIdentityMaterialV1::new(
            owner.clone(),
            FactIdentitySourceV1::Application {
                operation_id: id::<ProvenanceId>(operation),
            },
        )
        .unwrap(),
    )
    .unwrap()
}

#[cfg(test)]
fn relation_receipt(source_label: &str, metadata: &Value) -> SanitizationReceiptV1 {
    let material = serde_json::json!({
        "source_label": source_label,
        "metadata": metadata,
    });
    SanitizationReceiptV1::new(
        SanitizationReceiptRefV1::new(
            id::<SanitizationReceiptId>("receipt.relation.fixture"),
            id::<ComponentVersion>("sanitizer.fixture.v1"),
        )
        .unwrap(),
        SanitizerDispositionV1::Accepted,
        SensitivityV1::NonSensitive,
        Some(PayloadReferenceV1::for_payload(&material).unwrap()),
    )
    .unwrap()
}

#[cfg(test)]
fn relation_provenance(source_label: &str, metadata: Value) -> FactRelationProvenanceV1 {
    let receipt = relation_receipt(source_label, &metadata);
    FactRelationProvenanceV1::new(source_label.to_owned(), metadata, receipt).unwrap()
}

#[cfg(test)]
pub(in crate::memory) fn relation_evidence(owner: &FactOwnerV1) -> Vec<FactId> {
    let mut evidence = vec![
        fact_id_for(owner, "operation.relation.evidence.b"),
        fact_id_for(owner, "operation.relation.evidence.a"),
    ];
    evidence.sort_unstable();
    evidence
}

#[cfg(test)]
pub(in crate::memory) fn new_relation(
    owner: FactOwnerV1,
    source_fact_id: FactId,
    target_fact_id: FactId,
    kind: FactRelationKindV1,
    evidence_fact_ids: Vec<FactId>,
) -> Result<FactRelationV1, DomainError> {
    FactRelationV1::new(
        owner,
        source_fact_id,
        target_fact_id,
        kind,
        evidence_fact_ids,
        Confidence::new(0.8).unwrap(),
        relation_provenance(
            "curation.fixture",
            serde_json::json!({"provider": "fixture"}),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fact_relation_rejects_self_and_cross_owner_material() {
        let owner = FactOwnerV1::Profile;
        let source_fact_id = fact_id_for(&owner, "operation.relation.source");
        let target_fact_id = fact_id_for(&owner, "operation.relation.target");
        let evidence_fact_ids = relation_evidence(&owner);
        assert!(
            new_relation(
                owner.clone(),
                source_fact_id.clone(),
                source_fact_id.clone(),
                FactRelationKindV1::Supports,
                evidence_fact_ids.clone(),
            )
            .is_err()
        );

        let foreign_owner = FactOwnerV1::Project {
            project_id: id("project.foreign"),
        };
        let foreign_fact_id = fact_id_for(&foreign_owner, "operation.relation.foreign");
        assert!(
            new_relation(
                owner.clone(),
                source_fact_id.clone(),
                foreign_fact_id.clone(),
                FactRelationKindV1::Supports,
                evidence_fact_ids.clone(),
            )
            .is_err()
        );
        let mut foreign_evidence = evidence_fact_ids;
        foreign_evidence.push(foreign_fact_id);
        foreign_evidence.sort_unstable();
        assert!(
            new_relation(
                owner,
                source_fact_id,
                target_fact_id,
                FactRelationKindV1::Supports,
                foreign_evidence,
            )
            .is_err()
        );
    }

    #[test]
    fn fact_relation_rejects_noncanonical_evidence_order_and_duplicates() {
        let owner = FactOwnerV1::Profile;
        let source_fact_id = fact_id_for(&owner, "operation.relation.source");
        let target_fact_id = fact_id_for(&owner, "operation.relation.target");
        let evidence_fact_ids = relation_evidence(&owner);
        let mut unsorted = evidence_fact_ids.clone();
        unsorted.reverse();
        assert!(
            new_relation(
                owner.clone(),
                source_fact_id.clone(),
                target_fact_id.clone(),
                FactRelationKindV1::DerivedFrom,
                unsorted,
            )
            .is_err()
        );
        assert!(
            new_relation(
                owner.clone(),
                source_fact_id.clone(),
                target_fact_id.clone(),
                FactRelationKindV1::DerivedFrom,
                vec![evidence_fact_ids[0].clone(), evidence_fact_ids[0].clone()],
            )
            .is_err()
        );
        assert!(
            new_relation(
                owner,
                source_fact_id,
                target_fact_id,
                FactRelationKindV1::DerivedFrom,
                vec![],
            )
            .is_err()
        );
    }

    #[test]
    fn fact_relation_rejects_invalid_label_provenance_and_confidence() {
        let metadata = serde_json::json!({"provider": "fixture"});
        for source_label in ["", " untrimmed", "control\n"] {
            let receipt = relation_receipt(source_label, &metadata);
            assert!(
                FactRelationProvenanceV1::new(source_label.to_owned(), metadata.clone(), receipt,)
                    .is_err()
            );
        }
        let oversized_label = "x".repeat(MAX_FACT_RELATION_SOURCE_LABEL_BYTES + 1);
        let receipt = relation_receipt(&oversized_label, &metadata);
        assert!(FactRelationProvenanceV1::new(oversized_label, metadata.clone(), receipt).is_err());

        let mismatched_receipt = relation_receipt("curation.fixture", &metadata);
        assert!(
            FactRelationProvenanceV1::new(
                "curation.fixture".to_owned(),
                serde_json::json!({"provider": "other"}),
                mismatched_receipt,
            )
            .is_err()
        );
        let oversized_metadata = serde_json::json!({
            "value": "x".repeat(MAX_FACT_RELATION_METADATA_BYTES),
        });
        let receipt = relation_receipt("curation.fixture", &oversized_metadata);
        assert!(
            FactRelationProvenanceV1::new(
                "curation.fixture".to_owned(),
                oversized_metadata,
                receipt,
            )
            .is_err()
        );
        let max_label = "l".repeat(MAX_FACT_RELATION_SOURCE_LABEL_BYTES);
        let max_metadata = Value::String("m".repeat(MAX_FACT_RELATION_METADATA_BYTES - 2));
        let receipt = relation_receipt(&max_label, &max_metadata);
        assert!(FactRelationProvenanceV1::new(max_label, max_metadata, receipt).is_ok());
        let mut provenance_wire =
            serde_json::to_value(relation_provenance("curation.fixture", metadata)).unwrap();
        provenance_wire["sanitization_receipt"]["disposition"] = serde_json::json!("rejected");
        assert!(serde_json::from_value::<FactRelationProvenanceV1>(provenance_wire).is_err());

        let owner = FactOwnerV1::Profile;
        let relation = new_relation(
            owner.clone(),
            fact_id_for(&owner, "operation.relation.source"),
            fact_id_for(&owner, "operation.relation.target"),
            FactRelationKindV1::Contradicts,
            relation_evidence(&owner),
        )
        .unwrap();
        let mut wire = serde_json::to_value(relation).unwrap();
        wire["confidence"] = serde_json::json!(1.1);
        assert!(serde_json::from_value::<FactRelationV1>(wire).is_err());
    }
}
