use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::derive_memory_id;
use crate::observation::{ObservationScopeV1, PayloadReferenceV1, SanitizationReceiptV1};
use crate::research::{
    ActorId, Confidence, DomainError, EvidenceClass, FactAssertionId, FactEvidenceId, FactId,
    LocatorDigest, ProjectId, ProvenanceId, RetentionClass, RetrievalAnchorId, UtcMicros,
    validate_evidence_confidence,
};

const MAX_FACT_CONTENT_BYTES: usize = 64 * 1024;
const MAX_FACT_METADATA_BYTES: usize = 64 * 1024;
const MAX_FACT_SOURCE_LABEL_BYTES: usize = 4 * 1024;
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
pub struct FactPayloadV1 {
    content: String,
    category: FactCategoryV1,
    tags: Vec<String>,
    entities: Vec<String>,
    metadata: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_label: Option<String>,
    receipt: SanitizationReceiptV1,
    retention_class: RetentionClass,
}

#[derive(Serialize)]
struct FactPayloadMaterial<'a> {
    content: &'a str,
    category: FactCategoryV1,
    tags: &'a [String],
    entities: &'a [String],
    metadata: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_label: Option<&'a str>,
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
    /// Validates and canonicalizes the exact receipt-bound payload material.
    /// Tags and entities are sorted in place so every caller hashes and stores
    /// the same material rather than accepting order-dependent identities.
    pub fn canonicalize_material(
        content: &str,
        category: FactCategoryV1,
        tags: &mut Vec<String>,
        entities: &mut Vec<String>,
        metadata: &Value,
        source_label: Option<&str>,
    ) -> Result<PayloadReferenceV1, DomainError> {
        validate_content(content)?;
        validate_labels(tags, "fact tags")?;
        validate_labels(entities, "fact entities")?;
        tags.sort_unstable();
        entities.sort_unstable();
        let metadata_bytes = crate::research::canonical_json_bytes(metadata)?;
        if metadata_bytes.len() > MAX_FACT_METADATA_BYTES {
            return Err(DomainError::NonCanonical {
                field: "fact metadata",
            });
        }
        if source_label.is_some_and(|value| {
            !crate::canonical_text::is_canonical_text_within(value, MAX_FACT_SOURCE_LABEL_BYTES)
        }) {
            return Err(DomainError::NonCanonical {
                field: "fact source label",
            });
        }
        FactPayloadMaterial {
            content,
            category,
            tags,
            entities,
            metadata,
            source_label,
        }
        .payload_reference()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new(
        content: String,
        category: FactCategoryV1,
        mut tags: Vec<String>,
        mut entities: Vec<String>,
        metadata: Value,
        source_label: Option<String>,
        receipt: SanitizationReceiptV1,
        retention_class: RetentionClass,
    ) -> Result<Self, DomainError> {
        let payload_reference = Self::canonicalize_material(
            &content,
            category,
            &mut tags,
            &mut entities,
            &metadata,
            source_label.as_deref(),
        )?;
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
            source_label,
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

    pub fn source_label(&self) -> Option<&str> {
        self.source_label.as_deref()
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
            source_label: self.source_label.as_deref(),
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
            #[serde(default)]
            source_label: Option<String>,
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
            wire.source_label,
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
            Self::Initial => Ok(()),
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
            FactAssertionKindV1::Initial => false,
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
#[path = "fact_tests.rs"]
mod tests;
