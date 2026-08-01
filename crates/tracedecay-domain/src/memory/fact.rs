use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::derive_memory_id;
use crate::observation::{ObservationScopeV1, PayloadReferenceV1, SanitizationReceiptV1};
use crate::research::{
    ActorId, Confidence, DomainError, EvidenceClass, FactAssertionId, FactEvidenceId, FactId,
    LocatorDigest, ProjectId, ProvenanceId, RetentionClass, RetrievalAnchorId, SourceStoreId,
    UtcMicros, validate_evidence_confidence,
};

const MAX_FACT_CONTENT_BYTES: usize = 64 * 1024;
const MAX_FACT_METADATA_BYTES: usize = 64 * 1024;
const MAX_FACT_LABELS: usize = 64;
const MAX_FACT_LABEL_BYTES: usize = 512;
const MAX_FACT_EVIDENCE_REFS: usize = 256;
const MAX_ASSERTION_SUPERSEDES: usize = 256;
const FACT_ID_NAMESPACE: &str = "fact.v1";
const FACT_OWNER_NAMESPACE: &str = "fact-owner.v1";

/// Canonical storage owner. A profile owner denotes the one resolved user
/// profile; project facts carry their immutable project identity explicitly.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactOwnerV1 {
    Profile,
    Project { project_id: ProjectId },
}

impl FactOwnerV1 {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Profile => Ok(()),
            Self::Project { project_id } => project_id.validate(),
        }
    }
}

impl From<ObservationScopeV1> for FactOwnerV1 {
    fn from(scope: ObservationScopeV1) -> Self {
        match scope {
            ObservationScopeV1::Profile => Self::Profile,
            ObservationScopeV1::Project { project_id } => Self::Project { project_id },
        }
    }
}

impl From<FactOwnerV1> for ObservationScopeV1 {
    fn from(owner: FactOwnerV1) -> Self {
        match owner {
            FactOwnerV1::Profile => Self::Profile,
            FactOwnerV1::Project { project_id } => Self::Project { project_id },
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FactCategoryV1 {
    General,
    UserPref,
    Project,
    Tool,
    Decision,
    CodeArea,
}

/// Stable source material from which a fact identity is derived. Mutable text,
/// paths, ranks, and timestamps are deliberately excluded.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactIdentitySourceV1 {
    Evidence {
        anchor_id: RetrievalAnchorId,
        stable_key: LocatorDigest,
    },
    Application {
        operation_id: ProvenanceId,
    },
    Legacy {
        source_store_id: SourceStoreId,
        legacy_fact_id: i64,
    },
}

impl FactIdentitySourceV1 {
    fn validate(&self) -> Result<(), DomainError> {
        match self {
            Self::Evidence {
                anchor_id,
                stable_key,
            } => {
                anchor_id.validate()?;
                stable_key.validate()
            }
            Self::Application { operation_id } => operation_id.validate(),
            Self::Legacy {
                source_store_id,
                legacy_fact_id,
            } => {
                source_store_id.validate()?;
                if *legacy_fact_id <= 0 {
                    return Err(DomainError::NonCanonical {
                        field: "legacy fact id",
                    });
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactIdentityMaterialV1 {
    owner: FactOwnerV1,
    source: FactIdentitySourceV1,
}

impl FactIdentityMaterialV1 {
    pub fn new(owner: FactOwnerV1, source: FactIdentitySourceV1) -> Result<Self, DomainError> {
        owner.validate()?;
        source.validate()?;
        Ok(Self { owner, source })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn source(&self) -> &FactIdentitySourceV1 {
        &self.source
    }
}

impl FactId {
    pub fn derive(material: &FactIdentityMaterialV1) -> Result<Self, DomainError> {
        material.owner.validate()?;
        material.source.validate()?;
        let owner_binding = memory_id_suffix(
            FACT_OWNER_NAMESPACE,
            &derive_memory_id(FACT_OWNER_NAMESPACE, material.owner())?,
        )?;
        let identity = memory_id_suffix(
            FACT_ID_NAMESPACE,
            &derive_memory_id(FACT_ID_NAMESPACE, material)?,
        )?;
        Self::new(format!("{FACT_ID_NAMESPACE}.{owner_binding}.{identity}"))
    }

    /// Verify that this identity belongs to the supplied canonical owner.
    pub fn validate_owner(&self, owner: &FactOwnerV1) -> Result<(), DomainError> {
        validate_fact_owner(self, owner)
    }
}

fn validate_fact_owner(fact_id: &FactId, owner: &FactOwnerV1) -> Result<(), DomainError> {
    fact_id.validate()?;
    owner.validate()?;
    let encoded =
        strip_namespace(FACT_ID_NAMESPACE, fact_id.as_str()).ok_or(DomainError::NonCanonical {
            field: "fact identity",
        })?;
    let (claimed_owner, identity) = encoded.split_once('.').ok_or(DomainError::NonCanonical {
        field: "fact identity",
    })?;
    validate_sha256_hex(claimed_owner, "fact owner binding")?;
    validate_sha256_hex(identity, "fact identity")?;
    let expected_owner = memory_id_suffix(
        FACT_OWNER_NAMESPACE,
        &derive_memory_id(FACT_OWNER_NAMESPACE, owner)?,
    )?;
    if claimed_owner != expected_owner {
        return Err(DomainError::UnknownReference {
            field: "fact owner binding",
        });
    }
    Ok(())
}

/// Receipt-bound payload for one immutable assertion.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactPayloadV1 {
    content: String,
    category: FactCategoryV1,
    tags: Vec<String>,
    entities: Vec<String>,
    metadata: Value,
    receipt: SanitizationReceiptV1,
    retention_class: RetentionClass,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct FactPayloadMaterial<'a> {
    content: &'a str,
    category: FactCategoryV1,
    tags: &'a [String],
    entities: &'a [String],
    metadata: &'a Value,
}

impl FactPayloadMaterial<'_> {
    fn payload_reference(&self) -> Result<PayloadReferenceV1, DomainError> {
        let value = serde_json::to_value(self).map_err(|_| DomainError::NonCanonical {
            field: "fact payload",
        })?;
        PayloadReferenceV1::for_payload(&value).map_err(|_| DomainError::NonCanonical {
            field: "fact payload",
        })
    }
}

impl FactPayloadV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        content: String,
        category: FactCategoryV1,
        mut tags: Vec<String>,
        mut entities: Vec<String>,
        metadata: Value,
        receipt: SanitizationReceiptV1,
        retention_class: RetentionClass,
    ) -> Result<Self, DomainError> {
        validate_content(&content)?;
        validate_labels(&tags, "fact tags")?;
        validate_labels(&entities, "fact entities")?;
        tags.sort_unstable();
        entities.sort_unstable();
        let metadata_bytes = crate::research::canonical_json_bytes(&metadata)?;
        if metadata_bytes.len() > MAX_FACT_METADATA_BYTES {
            return Err(DomainError::NonCanonical {
                field: "fact metadata",
            });
        }
        let material = FactPayloadMaterial {
            content: &content,
            category,
            tags: &tags,
            entities: &entities,
            metadata: &metadata,
        };
        let payload_reference = material.payload_reference()?;
        if receipt.payload() != Some(&payload_reference) {
            return Err(DomainError::SnapshotMismatch {
                field: "fact sanitization receipt payload",
            });
        }
        Ok(Self {
            content,
            category,
            tags,
            entities,
            metadata,
            receipt,
            retention_class,
        })
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn category(&self) -> FactCategoryV1 {
        self.category
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn entities(&self) -> &[String] {
        &self.entities
    }

    pub fn metadata(&self) -> &Value {
        &self.metadata
    }

    pub fn receipt(&self) -> &SanitizationReceiptV1 {
        &self.receipt
    }

    pub fn retention_class(&self) -> &RetentionClass {
        &self.retention_class
    }

    pub fn payload_reference(&self) -> Result<PayloadReferenceV1, DomainError> {
        FactPayloadMaterial {
            content: &self.content,
            category: self.category,
            tags: &self.tags,
            entities: &self.entities,
            metadata: &self.metadata,
        }
        .payload_reference()
    }
}

impl<'de> Deserialize<'de> for FactPayloadV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            content: String,
            category: FactCategoryV1,
            tags: Vec<String>,
            entities: Vec<String>,
            metadata: Value,
            receipt: SanitizationReceiptV1,
            retention_class: RetentionClass,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.content,
            wire.category,
            wire.tags,
            wire.entities,
            wire.metadata,
            wire.receipt,
            wire.retention_class,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum FactEvidenceRelationV1 {
    Supports,
    Contradicts,
    DerivedFrom,
    CopiedFrom,
    Corrects,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactEvidenceRefV1 {
    evidence_id: FactEvidenceId,
    fact_id: FactId,
    anchor_id: RetrievalAnchorId,
    relation: FactEvidenceRelationV1,
    evidence_class: EvidenceClass,
    confidence: Confidence,
}

#[derive(Serialize)]
struct FactEvidenceIdentityMaterial<'a> {
    fact_id: &'a FactId,
    anchor_id: &'a RetrievalAnchorId,
    relation: FactEvidenceRelationV1,
    evidence_class: EvidenceClass,
    confidence: Confidence,
}

impl FactEvidenceRefV1 {
    pub fn new(
        fact_id: FactId,
        anchor_id: RetrievalAnchorId,
        relation: FactEvidenceRelationV1,
        evidence_class: EvidenceClass,
        confidence: Confidence,
    ) -> Result<Self, DomainError> {
        fact_id.validate()?;
        anchor_id.validate()?;
        validate_evidence_confidence(evidence_class, confidence)?;
        let evidence_id = FactEvidenceId::new(derive_memory_id(
            "fact-evidence.v1",
            &FactEvidenceIdentityMaterial {
                fact_id: &fact_id,
                anchor_id: &anchor_id,
                relation,
                evidence_class,
                confidence,
            },
        )?)?;
        Ok(Self {
            evidence_id,
            fact_id,
            anchor_id,
            relation,
            evidence_class,
            confidence,
        })
    }

    pub fn evidence_id(&self) -> &FactEvidenceId {
        &self.evidence_id
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn anchor_id(&self) -> &RetrievalAnchorId {
        &self.anchor_id
    }

    pub fn relation(&self) -> FactEvidenceRelationV1 {
        self.relation
    }

    pub fn evidence_class(&self) -> EvidenceClass {
        self.evidence_class
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

impl<'de> Deserialize<'de> for FactEvidenceRefV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            evidence_id: FactEvidenceId,
            fact_id: FactId,
            anchor_id: RetrievalAnchorId,
            relation: FactEvidenceRelationV1,
            evidence_class: EvidenceClass,
            confidence: Confidence,
        }

        let wire = Wire::deserialize(deserializer)?;
        let claimed_id = wire.evidence_id;
        let evidence = Self::new(
            wire.fact_id,
            wire.anchor_id,
            wire.relation,
            wire.evidence_class,
            wire.confidence,
        )
        .map_err(serde::de::Error::custom)?;
        if claimed_id != evidence.evidence_id {
            return Err(serde::de::Error::custom(DomainError::DigestMismatch));
        }
        Ok(evidence)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FactAssertionKindV1 {
    Initial,
    Correction { supersedes: FactAssertionId },
    Merge { supersedes: Vec<FactAssertionId> },
    LegacyImport,
}

impl FactAssertionKindV1 {
    fn canonicalized(mut self) -> Result<Self, DomainError> {
        match &mut self {
            Self::Correction { supersedes } => supersedes.validate(),
            Self::Merge { supersedes } => {
                if supersedes.is_empty() {
                    return Err(DomainError::Empty {
                        field: "merged assertions",
                    });
                }
                if supersedes.len() > MAX_ASSERTION_SUPERSEDES {
                    return Err(DomainError::NonCanonical {
                        field: "merged assertions",
                    });
                }
                supersedes.sort_unstable();
                validate_unique(supersedes.iter(), "merged assertions")?;
                for assertion_id in supersedes {
                    assertion_id.validate()?;
                }
                Ok(())
            }
            Self::Initial | Self::LegacyImport => Ok(()),
        }?;
        Ok(self)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FactAssertionV1 {
    assertion_id: FactAssertionId,
    fact_id: FactId,
    owner: FactOwnerV1,
    kind: FactAssertionKindV1,
    payload: FactPayloadV1,
    evidence: Vec<FactEvidenceRefV1>,
    asserted_at: UtcMicros,
    actor_id: Option<ActorId>,
}

#[derive(Serialize)]
struct FactAssertionIdentityMaterial<'a> {
    fact_id: &'a FactId,
    owner: &'a FactOwnerV1,
    kind: &'a FactAssertionKindV1,
    payload_reference: &'a PayloadReferenceV1,
    evidence_ids: Vec<&'a FactEvidenceId>,
    asserted_at: UtcMicros,
    actor_id: Option<&'a ActorId>,
}

impl FactAssertionV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fact_id: FactId,
        owner: FactOwnerV1,
        kind: FactAssertionKindV1,
        payload: FactPayloadV1,
        mut evidence: Vec<FactEvidenceRefV1>,
        asserted_at: UtcMicros,
        actor_id: Option<ActorId>,
    ) -> Result<Self, DomainError> {
        fact_id.validate()?;
        owner.validate()?;
        fact_id.validate_owner(&owner)?;
        let kind = kind.canonicalized()?;
        if evidence.len() > MAX_FACT_EVIDENCE_REFS {
            return Err(DomainError::NonCanonical {
                field: "fact assertion evidence",
            });
        }
        if let Some(actor_id) = &actor_id {
            actor_id.validate()?;
        }
        for item in &evidence {
            if item.fact_id() != &fact_id {
                return Err(DomainError::UnknownReference {
                    field: "fact assertion evidence fact",
                });
            }
        }
        evidence.sort_unstable_by(|left, right| left.evidence_id.cmp(&right.evidence_id));
        validate_unique(
            evidence.iter().map(FactEvidenceRefV1::evidence_id),
            "fact assertion evidence",
        )?;
        let payload_reference = payload.payload_reference()?;
        let evidence_ids = evidence
            .iter()
            .map(FactEvidenceRefV1::evidence_id)
            .collect();
        let assertion_id = FactAssertionId::new(derive_memory_id(
            "fact-assertion.v1",
            &FactAssertionIdentityMaterial {
                fact_id: &fact_id,
                owner: &owner,
                kind: &kind,
                payload_reference: &payload_reference,
                evidence_ids,
                asserted_at,
                actor_id: actor_id.as_ref(),
            },
        )?)?;
        if match &kind {
            FactAssertionKindV1::Correction { supersedes } => supersedes == &assertion_id,
            FactAssertionKindV1::Merge { supersedes } => supersedes.contains(&assertion_id),
            FactAssertionKindV1::Initial | FactAssertionKindV1::LegacyImport => false,
        } {
            return Err(DomainError::SelfSupersession);
        }
        Ok(Self {
            assertion_id,
            fact_id,
            owner,
            kind,
            payload,
            evidence,
            asserted_at,
            actor_id,
        })
    }

    pub fn assertion_id(&self) -> &FactAssertionId {
        &self.assertion_id
    }

    pub fn fact_id(&self) -> &FactId {
        &self.fact_id
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn kind(&self) -> &FactAssertionKindV1 {
        &self.kind
    }

    pub fn payload(&self) -> &FactPayloadV1 {
        &self.payload
    }

    pub fn evidence(&self) -> &[FactEvidenceRefV1] {
        &self.evidence
    }

    pub fn asserted_at(&self) -> UtcMicros {
        self.asserted_at
    }

    pub fn actor_id(&self) -> Option<&ActorId> {
        self.actor_id.as_ref()
    }
}

impl<'de> Deserialize<'de> for FactAssertionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            assertion_id: FactAssertionId,
            fact_id: FactId,
            owner: FactOwnerV1,
            kind: FactAssertionKindV1,
            payload: FactPayloadV1,
            evidence: Vec<FactEvidenceRefV1>,
            asserted_at: UtcMicros,
            actor_id: Option<ActorId>,
        }

        let wire = Wire::deserialize(deserializer)?;
        let claimed_id = wire.assertion_id;
        let assertion = Self::new(
            wire.fact_id,
            wire.owner,
            wire.kind,
            wire.payload,
            wire.evidence,
            wire.asserted_at,
            wire.actor_id,
        )
        .map_err(serde::de::Error::custom)?;
        if claimed_id != assertion.assertion_id {
            return Err(serde::de::Error::custom(DomainError::DigestMismatch));
        }
        Ok(assertion)
    }
}

fn validate_content(content: &str) -> Result<(), DomainError> {
    if content.trim().is_empty() || content.len() > MAX_FACT_CONTENT_BYTES {
        return Err(DomainError::NonCanonical {
            field: "fact content",
        });
    }
    Ok(())
}

fn validate_labels(values: &[String], field: &'static str) -> Result<(), DomainError> {
    if values.len() > MAX_FACT_LABELS {
        return Err(DomainError::NonCanonical { field });
    }
    for value in values {
        if !crate::canonical_text::is_canonical_text_within(value, MAX_FACT_LABEL_BYTES) {
            return Err(DomainError::NonCanonical { field });
        }
    }
    validate_unique(values.iter(), field)
}

fn validate_unique<'a, T: 'a + Ord>(
    values: impl IntoIterator<Item = &'a T>,
    field: &'static str,
) -> Result<(), DomainError> {
    let mut seen = BTreeSet::new();
    if values.into_iter().any(|value| !seen.insert(value)) {
        return Err(DomainError::DuplicateId { field });
    }
    Ok(())
}

/// Strip a `"{namespace}."` prefix without allocating the prefix to match on.
fn strip_namespace<'a>(namespace: &str, value: &'a str) -> Option<&'a str> {
    value
        .strip_prefix(namespace)
        .and_then(|rest| rest.strip_prefix('.'))
}

fn memory_id_suffix(namespace: &'static str, value: &str) -> Result<String, DomainError> {
    strip_namespace(namespace, value)
        .map(str::to_owned)
        .ok_or(DomainError::NonCanonical {
            field: "memory identity",
        })
}

fn validate_sha256_hex(value: &str, field: &'static str) -> Result<(), DomainError> {
    if crate::canonical_text::is_lowercase_hex(value, 64) {
        Ok(())
    } else {
        Err(DomainError::NonCanonical { field })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::observation::{SanitizerDispositionV1, SensitivityV1};
    use crate::research::{ComponentVersion, SanitizationReceiptId, SanitizationReceiptRefV1};

    const ZERO_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn id<T>(value: &str) -> T
    where
        T: TryFrom<String, Error = DomainError>,
    {
        T::try_from(value.to_owned()).unwrap()
    }

    fn fact_id(owner: FactOwnerV1, operation: &str) -> FactId {
        FactId::derive(
            &FactIdentityMaterialV1::new(
                owner,
                FactIdentitySourceV1::Application {
                    operation_id: id(operation),
                },
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn payload() -> FactPayloadV1 {
        let material = json!({
            "content": "The daemon is the only writer.",
            "category": "project",
            "tags": ["daemon", "database"],
            "entities": ["TraceDecay"],
            "metadata": {"source": "fixture"},
        });
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                id::<SanitizationReceiptId>("receipt.fact.fixture"),
                id::<ComponentVersion>("sanitizer.fixture.v1"),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(PayloadReferenceV1::for_payload(&material).unwrap()),
        )
        .unwrap();
        FactPayloadV1::new(
            "The daemon is the only writer.".to_owned(),
            FactCategoryV1::Project,
            vec!["daemon".to_owned(), "database".to_owned()],
            vec!["TraceDecay".to_owned()],
            json!({"source": "fixture"}),
            receipt,
            RetentionClass::new("durable.fact").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn fact_and_evidence_ids_are_deterministic_and_owner_scoped() {
        let project_owner = FactOwnerV1::Project {
            project_id: id("project.fixture"),
        };
        let first = fact_id(project_owner.clone(), "operation.fixture");
        let replay = fact_id(project_owner, "operation.fixture");
        let profile = fact_id(FactOwnerV1::Profile, "operation.fixture");
        assert_eq!(first, replay);
        assert_ne!(first, profile);

        let evidence = FactEvidenceRefV1::new(
            first.clone(),
            id("retrieval.fixture"),
            FactEvidenceRelationV1::Supports,
            EvidenceClass::Observed,
            Confidence::new(1.0).unwrap(),
        )
        .unwrap();
        let replayed = FactEvidenceRefV1::new(
            first.clone(),
            id("retrieval.fixture"),
            FactEvidenceRelationV1::Supports,
            EvidenceClass::Observed,
            Confidence::new(1.0).unwrap(),
        )
        .unwrap();
        assert_eq!(evidence.evidence_id(), replayed.evidence_id());

        let lower_confidence = FactEvidenceRefV1::new(
            first,
            id("retrieval.fixture"),
            FactEvidenceRelationV1::Supports,
            EvidenceClass::Inferred,
            Confidence::new(0.8).unwrap(),
        )
        .unwrap();
        assert_ne!(evidence.evidence_id(), lower_confidence.evidence_id());
    }

    #[test]
    fn assertion_identity_changes_with_owner_payload_and_lineage() {
        let owner = FactOwnerV1::Project {
            project_id: id("project.fixture"),
        };
        let fact_id = fact_id(owner.clone(), "operation.fixture");
        let evidence = FactEvidenceRefV1::new(
            fact_id.clone(),
            id("retrieval.fixture"),
            FactEvidenceRelationV1::Supports,
            EvidenceClass::Observed,
            Confidence::new(1.0).unwrap(),
        )
        .unwrap();
        let first = FactAssertionV1::new(
            fact_id.clone(),
            owner.clone(),
            FactAssertionKindV1::Initial,
            payload(),
            vec![evidence.clone()],
            UtcMicros(10),
            None,
        )
        .unwrap();
        let replay = FactAssertionV1::new(
            fact_id,
            owner,
            FactAssertionKindV1::Initial,
            payload(),
            vec![evidence],
            UtcMicros(10),
            None,
        )
        .unwrap();
        assert_eq!(first.assertion_id(), replay.assertion_id());
    }

    #[test]
    fn payload_rejects_an_unbound_receipt() {
        let wrong_reference = PayloadReferenceV1::for_payload(&json!({"different": true})).unwrap();
        let receipt = SanitizationReceiptV1::new(
            SanitizationReceiptRefV1::new(
                id::<SanitizationReceiptId>("receipt.fact.wrong"),
                id::<ComponentVersion>("sanitizer.fixture.v1"),
            )
            .unwrap(),
            SanitizerDispositionV1::Accepted,
            SensitivityV1::NonSensitive,
            Some(wrong_reference),
        )
        .unwrap();
        assert!(
            FactPayloadV1::new(
                "safe".to_owned(),
                FactCategoryV1::General,
                vec![],
                vec![],
                json!({}),
                receipt,
                RetentionClass::new("durable.fact").unwrap(),
            )
            .is_err()
        );
    }

    #[test]
    fn evidence_cannot_be_attached_to_another_fact() {
        let owner = FactOwnerV1::Profile;
        let first = fact_id(owner.clone(), "operation.first");
        let second = fact_id(owner.clone(), "operation.second");
        let evidence = FactEvidenceRefV1::new(
            first,
            id("retrieval.fixture"),
            FactEvidenceRelationV1::Supports,
            EvidenceClass::Observed,
            Confidence::new(1.0).unwrap(),
        )
        .unwrap();
        assert!(
            FactAssertionV1::new(
                second,
                owner,
                FactAssertionKindV1::Initial,
                payload(),
                vec![evidence],
                UtcMicros(10),
                None,
            )
            .is_err()
        );
    }

    #[test]
    fn identity_bearing_wire_values_reject_tampering() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id(owner.clone(), "operation.wire");
        let evidence = FactEvidenceRefV1::new(
            fact_id.clone(),
            id("retrieval.wire"),
            FactEvidenceRelationV1::Supports,
            EvidenceClass::Observed,
            Confidence::new(1.0).unwrap(),
        )
        .unwrap();
        let mut evidence_wire = serde_json::to_value(&evidence).unwrap();
        evidence_wire["evidence_id"] = json!("fact-evidence.v1.forged");
        assert!(serde_json::from_value::<FactEvidenceRefV1>(evidence_wire).is_err());

        let assertion = FactAssertionV1::new(
            fact_id,
            owner,
            FactAssertionKindV1::Initial,
            payload(),
            vec![evidence],
            UtcMicros(10),
            None,
        )
        .unwrap();
        let mut assertion_wire = serde_json::to_value(&assertion).unwrap();
        assertion_wire["assertion_id"] = json!("fact-assertion.v1.forged");
        assert!(serde_json::from_value::<FactAssertionV1>(assertion_wire).is_err());

        let mut owner_wire = serde_json::to_value(&assertion).unwrap();
        owner_wire["owner"] = json!({"kind": "project", "project_id": "project.other"});
        assert!(serde_json::from_value::<FactAssertionV1>(owner_wire).is_err());
    }

    #[test]
    fn assertion_set_order_is_canonical() {
        let owner = FactOwnerV1::Profile;
        let fact_id = fact_id(owner.clone(), "operation.order");
        let first_evidence = FactEvidenceRefV1::new(
            fact_id.clone(),
            id("retrieval.order.a"),
            FactEvidenceRelationV1::Supports,
            EvidenceClass::Observed,
            Confidence::new(1.0).unwrap(),
        )
        .unwrap();
        let second_evidence = FactEvidenceRefV1::new(
            fact_id.clone(),
            id("retrieval.order.b"),
            FactEvidenceRelationV1::Supports,
            EvidenceClass::Observed,
            Confidence::new(1.0).unwrap(),
        )
        .unwrap();
        let first = FactAssertionV1::new(
            fact_id.clone(),
            owner.clone(),
            FactAssertionKindV1::Initial,
            payload(),
            vec![first_evidence.clone(), second_evidence.clone()],
            UtcMicros(10),
            None,
        )
        .unwrap();
        let second = FactAssertionV1::new(
            fact_id,
            owner,
            FactAssertionKindV1::Initial,
            payload(),
            vec![second_evidence, first_evidence],
            UtcMicros(10),
            None,
        )
        .unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn legacy_identity_rejects_non_positive_row_ids() {
        assert!(
            FactIdentityMaterialV1::new(
                FactOwnerV1::Profile,
                FactIdentitySourceV1::Legacy {
                    source_store_id: id("store.fixture"),
                    legacy_fact_id: 0,
                },
            )
            .is_err()
        );
        assert!(LocatorDigest::new(ZERO_DIGEST).is_ok());
    }
}
