use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use tracedecay_domain::{
    ActorId, Confidence, DomainError, FactOwnerV1, PayloadReferenceV1, ProvenanceId,
    SanitizationReceiptV1, SanitizerDispositionV1,
};

use super::super::super::queries::validate_limit;
use super::super::super::{FactStoreError, FactStoreResult, ProjectMemoryMemoryRepairStatsV1};
use super::super::{
    ProjectMemoryFactMappingV1, ProjectMemoryFactTargetV1, validate_compatibility_metadata,
    validate_compatibility_text,
};
use super::validate::{
    validate_curation_confidence, validate_curation_entity_target, validate_curation_evidence,
    validate_curation_fact_target,
};
use super::{MAX_COMPATIBILITY_CURATION_OPERATIONS, MAX_COMPATIBILITY_CURATION_TARGETS};

/// Stable, owner-scoped identity for a historical integer entity row. This is
/// only a compatibility target; it is never derived from a path or label.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProjectMemoryLegacyEntityTargetV1 {
    owner: FactOwnerV1,
    legacy_entity_id: i64,
}

impl ProjectMemoryLegacyEntityTargetV1 {
    pub fn new(owner: FactOwnerV1, legacy_entity_id: i64) -> FactStoreResult<Self> {
        owner.validate()?;
        if legacy_entity_id <= 0 {
            return Err(FactStoreError::InvalidLegacyFactId {
                legacy_fact_id: legacy_entity_id,
            });
        }
        Ok(Self {
            owner,
            legacy_entity_id,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn legacy_entity_id(&self) -> i64 {
        self.legacy_entity_id
    }

    pub(in crate::memory::project_memory) fn validate(&self) -> FactStoreResult<()> {
        self.owner.validate()?;
        if self.legacy_entity_id <= 0 {
            return Err(FactStoreError::InvalidLegacyFactId {
                legacy_fact_id: self.legacy_entity_id,
            });
        }
        Ok(())
    }
}

/// The finite relationship vocabulary supported by legacy dashboard curation.
/// `Supports` and `DerivedFrom` are persisted as typed relations rather than
/// being misrepresented as a canonical lineage action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectMemoryFactRelationV1 {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactNormalizeTagsV1 {
    fact: ProjectMemoryFactTargetV1,
    tags: Vec<String>,
    evidence_facts: Vec<ProjectMemoryFactTargetV1>,
    confidence: Confidence,
}

impl ProjectMemoryFactNormalizeTagsV1 {
    pub fn new(
        fact: ProjectMemoryFactTargetV1,
        tags: Vec<String>,
        evidence_facts: Vec<ProjectMemoryFactTargetV1>,
        confidence: Confidence,
    ) -> FactStoreResult<Self> {
        if tags.len() > MAX_COMPATIBILITY_CURATION_TARGETS {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: tags.len(),
                max: MAX_COMPATIBILITY_CURATION_TARGETS,
            });
        }
        for tag in &tags {
            validate_compatibility_text(tag, "compatibility curation tag")?;
        }
        Ok(Self {
            fact,
            tags,
            evidence_facts,
            confidence,
        })
    }

    pub fn fact(&self) -> &ProjectMemoryFactTargetV1 {
        &self.fact
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn evidence_facts(&self) -> &[ProjectMemoryFactTargetV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactMergeEntitiesV1 {
    winner: ProjectMemoryLegacyEntityTargetV1,
    losers: Vec<ProjectMemoryLegacyEntityTargetV1>,
    evidence_facts: Vec<ProjectMemoryFactTargetV1>,
    confidence: Confidence,
}

impl ProjectMemoryFactMergeEntitiesV1 {
    pub fn new(
        winner: ProjectMemoryLegacyEntityTargetV1,
        losers: Vec<ProjectMemoryLegacyEntityTargetV1>,
        evidence_facts: Vec<ProjectMemoryFactTargetV1>,
        confidence: Confidence,
    ) -> FactStoreResult<Self> {
        validate_entity_merge(&winner, &losers)?;
        Ok(Self {
            winner,
            losers,
            evidence_facts,
            confidence,
        })
    }

    pub fn winner(&self) -> &ProjectMemoryLegacyEntityTargetV1 {
        &self.winner
    }

    pub fn losers(&self) -> &[ProjectMemoryLegacyEntityTargetV1] {
        &self.losers
    }

    pub fn evidence_facts(&self) -> &[ProjectMemoryFactTargetV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactAddAliasV1 {
    entity: ProjectMemoryLegacyEntityTargetV1,
    alias: String,
    evidence_facts: Vec<ProjectMemoryFactTargetV1>,
    confidence: Confidence,
}

impl ProjectMemoryFactAddAliasV1 {
    pub fn new(
        entity: ProjectMemoryLegacyEntityTargetV1,
        alias: String,
        evidence_facts: Vec<ProjectMemoryFactTargetV1>,
        confidence: Confidence,
    ) -> FactStoreResult<Self> {
        validate_compatibility_text(&alias, "compatibility curation alias")?;
        Ok(Self {
            entity,
            alias,
            evidence_facts,
            confidence,
        })
    }

    pub fn entity(&self) -> &ProjectMemoryLegacyEntityTargetV1 {
        &self.entity
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn evidence_facts(&self) -> &[ProjectMemoryFactTargetV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

/// Receipt-bound sanitized relation metadata.
///
/// This complete value is the canonical relation provenance. Persistence
/// stores it once, rather than maintaining independent metadata and receipt
/// JSON authorities that can drift.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectMemoryRelationProvenanceV1 {
    metadata: Value,
    sanitization_receipt: SanitizationReceiptV1,
}

impl ProjectMemoryRelationProvenanceV1 {
    pub fn new(
        metadata: Value,
        sanitization_receipt: SanitizationReceiptV1,
    ) -> FactStoreResult<Self> {
        validate_compatibility_metadata(&metadata, "compatibility relation provenance metadata")?;
        if !matches!(
            sanitization_receipt.disposition(),
            SanitizerDispositionV1::Accepted | SanitizerDispositionV1::Redacted
        ) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility relation provenance sanitization disposition",
            }));
        }
        let payload_reference = PayloadReferenceV1::for_payload(&metadata).map_err(|_| {
            FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility relation provenance metadata",
            })
        })?;
        if sanitization_receipt.payload() != Some(&payload_reference) {
            return Err(FactStoreError::Contract(DomainError::SnapshotMismatch {
                field: "compatibility relation provenance sanitization receipt",
            }));
        }
        Ok(Self {
            metadata,
            sanitization_receipt,
        })
    }

    pub fn metadata(&self) -> &Value {
        &self.metadata
    }

    pub fn sanitization_receipt(&self) -> &SanitizationReceiptV1 {
        &self.sanitization_receipt
    }
}

impl<'de> Deserialize<'de> for ProjectMemoryRelationProvenanceV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            metadata: Value,
            sanitization_receipt: SanitizationReceiptV1,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.metadata, wire.sanitization_receipt).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactLinkV1 {
    source: ProjectMemoryFactTargetV1,
    target: ProjectMemoryFactTargetV1,
    relation: ProjectMemoryFactRelationV1,
    evidence_facts: Vec<ProjectMemoryFactTargetV1>,
    confidence: Confidence,
    source_label: String,
    provenance: ProjectMemoryRelationProvenanceV1,
}

impl ProjectMemoryFactLinkV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: ProjectMemoryFactTargetV1,
        target: ProjectMemoryFactTargetV1,
        relation: ProjectMemoryFactRelationV1,
        evidence_facts: Vec<ProjectMemoryFactTargetV1>,
        confidence: Confidence,
        source_label: String,
        provenance: ProjectMemoryRelationProvenanceV1,
    ) -> FactStoreResult<Self> {
        if source == target {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility curation relation endpoints",
            }));
        }
        validate_compatibility_text(&source_label, "compatibility curation relation source")?;
        Ok(Self {
            source,
            target,
            relation,
            evidence_facts,
            confidence,
            source_label,
            provenance,
        })
    }

    pub fn source(&self) -> &ProjectMemoryFactTargetV1 {
        &self.source
    }

    pub fn target(&self) -> &ProjectMemoryFactTargetV1 {
        &self.target
    }

    pub fn relation(&self) -> ProjectMemoryFactRelationV1 {
        self.relation
    }

    pub fn evidence_facts(&self) -> &[ProjectMemoryFactTargetV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub fn metadata(&self) -> &Value {
        self.provenance.metadata()
    }

    pub fn provenance(&self) -> &ProjectMemoryRelationProvenanceV1 {
        &self.provenance
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactRepairVectorV1 {
    fact: ProjectMemoryFactTargetV1,
    evidence_facts: Vec<ProjectMemoryFactTargetV1>,
    confidence: Confidence,
}

impl ProjectMemoryFactRepairVectorV1 {
    pub fn new(
        fact: ProjectMemoryFactTargetV1,
        evidence_facts: Vec<ProjectMemoryFactTargetV1>,
        confidence: Confidence,
    ) -> Self {
        Self {
            fact,
            evidence_facts,
            confidence,
        }
    }

    pub fn fact(&self) -> &ProjectMemoryFactTargetV1 {
        &self.fact
    }

    pub fn evidence_facts(&self) -> &[ProjectMemoryFactTargetV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

/// Finite set of curation operations; this is intentionally not a generic
/// command dispatcher.
#[derive(Clone, Debug, PartialEq)]
pub enum ProjectMemoryFactCurationOperationV1 {
    NormalizeTags(ProjectMemoryFactNormalizeTagsV1),
    MergeEntities(ProjectMemoryFactMergeEntitiesV1),
    AddAlias(ProjectMemoryFactAddAliasV1),
    LinkFacts(ProjectMemoryFactLinkV1),
    RepairVector(ProjectMemoryFactRepairVectorV1),
}

impl ProjectMemoryFactCurationOperationV1 {
    fn validate_for(&self, owner: &FactOwnerV1, min_confidence: Confidence) -> FactStoreResult<()> {
        match self {
            Self::NormalizeTags(operation) => {
                validate_curation_fact_target(owner, operation.fact())?;
                validate_curation_evidence(owner, operation.evidence_facts())?;
                validate_curation_confidence(operation.confidence(), min_confidence)
            }
            Self::MergeEntities(operation) => {
                validate_curation_entity_target(owner, operation.winner())?;
                for loser in operation.losers() {
                    validate_curation_entity_target(owner, loser)?;
                }
                validate_curation_evidence(owner, operation.evidence_facts())?;
                validate_curation_confidence(operation.confidence(), min_confidence)
            }
            Self::AddAlias(operation) => {
                validate_curation_entity_target(owner, operation.entity())?;
                validate_curation_evidence(owner, operation.evidence_facts())?;
                validate_curation_confidence(operation.confidence(), min_confidence)
            }
            Self::LinkFacts(operation) => {
                validate_curation_fact_target(owner, operation.source())?;
                validate_curation_fact_target(owner, operation.target())?;
                validate_curation_evidence(owner, operation.evidence_facts())?;
                validate_curation_confidence(operation.confidence(), min_confidence)
            }
            Self::RepairVector(operation) => {
                validate_curation_fact_target(owner, operation.fact())?;
                validate_curation_evidence(owner, operation.evidence_facts())?;
                validate_curation_confidence(operation.confidence(), min_confidence)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectMemoryFactCurationBatchV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
    min_confidence: Confidence,
    operations: Vec<ProjectMemoryFactCurationOperationV1>,
}

impl ProjectMemoryFactCurationBatchV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        actor: Option<ActorId>,
        min_confidence: Confidence,
        operations: Vec<ProjectMemoryFactCurationOperationV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        validate_limit(operations.len(), MAX_COMPATIBILITY_CURATION_OPERATIONS)?;
        for operation in &operations {
            operation.validate_for(&owner, min_confidence)?;
        }
        Ok(Self {
            owner,
            operation_id,
            actor,
            min_confidence,
            operations,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }

    pub fn min_confidence(&self) -> Confidence {
        self.min_confidence
    }

    pub fn operations(&self) -> &[ProjectMemoryFactCurationOperationV1] {
        &self.operations
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMemoryFactCurationReceiptV1 {
    owner: FactOwnerV1,
    changed_facts: Vec<ProjectMemoryFactMappingV1>,
    normalized_tags: u64,
    merged_entities: u64,
    aliases_added: u64,
    facts_linked: u64,
    vectors_repaired: u64,
    derived_repair: ProjectMemoryMemoryRepairStatsV1,
}

impl ProjectMemoryFactCurationReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        changed_facts: Vec<ProjectMemoryFactMappingV1>,
        normalized_tags: u64,
        merged_entities: u64,
        aliases_added: u64,
        facts_linked: u64,
        vectors_repaired: u64,
        derived_repair: ProjectMemoryMemoryRepairStatsV1,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if changed_facts.len() > MAX_COMPATIBILITY_CURATION_TARGETS
            || changed_facts
                .iter()
                .any(|mapping| mapping.owner() != &owner)
            || changed_facts.iter().enumerate().any(|(index, mapping)| {
                changed_facts[..index]
                    .iter()
                    .any(|previous| previous.fact_id() == mapping.fact_id())
            })
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility curation receipt mappings",
            }));
        }
        Ok(Self {
            owner,
            changed_facts,
            normalized_tags,
            merged_entities,
            aliases_added,
            facts_linked,
            vectors_repaired,
            derived_repair,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn changed_facts(&self) -> &[ProjectMemoryFactMappingV1] {
        &self.changed_facts
    }

    pub fn normalized_tags(&self) -> u64 {
        self.normalized_tags
    }

    pub fn merged_entities(&self) -> u64 {
        self.merged_entities
    }

    pub fn aliases_added(&self) -> u64 {
        self.aliases_added
    }

    pub fn facts_linked(&self) -> u64 {
        self.facts_linked
    }

    pub fn vectors_repaired(&self) -> u64 {
        self.vectors_repaired
    }

    pub fn derived_repair(&self) -> &ProjectMemoryMemoryRepairStatsV1 {
        &self.derived_repair
    }
}

fn validate_entity_merge(
    winner: &ProjectMemoryLegacyEntityTargetV1,
    losers: &[ProjectMemoryLegacyEntityTargetV1],
) -> FactStoreResult<()> {
    if losers.is_empty() || losers.len() > MAX_COMPATIBILITY_CURATION_TARGETS {
        return Err(FactStoreError::InvalidQueryLimit {
            limit: losers.len(),
            max: MAX_COMPATIBILITY_CURATION_TARGETS,
        });
    }
    for (index, loser) in losers.iter().enumerate() {
        if loser.owner() != winner.owner()
            || loser == winner
            || losers[..index].iter().any(|previous| previous == loser)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility curation entity merge",
            }));
        }
    }
    Ok(())
}
