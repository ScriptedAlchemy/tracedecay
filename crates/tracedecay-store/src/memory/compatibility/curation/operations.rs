use serde_json::Value;
use tracedecay_domain::{ActorId, Confidence, DomainError, FactOwnerV1, ProvenanceId};

use super::super::super::queries::validate_limit;
use super::super::super::{FactLineageError, FactLineageResult, MemoryRepairStats};
use super::super::{FactMapping, FactTarget, validate_metadata, validate_text};
use super::validate::{
    validate_curation_confidence, validate_curation_entity_target, validate_curation_evidence,
    validate_curation_fact_target,
};
use super::{MAX_FACT_CURATION_OPERATIONS, MAX_FACT_CURATION_TARGETS};

/// Stable, owner-scoped identity for a historical integer entity row. This is
/// only a compatibility target; it is never derived from a path or label.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FactEntityTarget {
    owner: FactOwnerV1,
    legacy_entity_id: i64,
}

impl FactEntityTarget {
    pub fn new(owner: FactOwnerV1, legacy_entity_id: i64) -> FactLineageResult<Self> {
        owner.validate()?;
        if legacy_entity_id <= 0 {
            return Err(FactLineageError::InvalidLegacyFactId {
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

    pub(in crate::memory::compatibility) fn validate(&self) -> FactLineageResult<()> {
        self.owner.validate()?;
        if self.legacy_entity_id <= 0 {
            return Err(FactLineageError::InvalidLegacyFactId {
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
pub enum FactRelation {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactNormalizeTags {
    fact: FactTarget,
    tags: Vec<String>,
    evidence_facts: Vec<FactTarget>,
    confidence: Confidence,
}

impl FactNormalizeTags {
    pub fn new(
        fact: FactTarget,
        tags: Vec<String>,
        evidence_facts: Vec<FactTarget>,
        confidence: Confidence,
    ) -> FactLineageResult<Self> {
        if tags.len() > MAX_FACT_CURATION_TARGETS {
            return Err(FactLineageError::InvalidQueryLimit {
                limit: tags.len(),
                max: MAX_FACT_CURATION_TARGETS,
            });
        }
        for tag in &tags {
            validate_text(tag, "compatibility curation tag")?;
        }
        Ok(Self {
            fact,
            tags,
            evidence_facts,
            confidence,
        })
    }

    pub fn fact(&self) -> &FactTarget {
        &self.fact
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn evidence_facts(&self) -> &[FactTarget] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactMergeEntities {
    winner: FactEntityTarget,
    losers: Vec<FactEntityTarget>,
    evidence_facts: Vec<FactTarget>,
    confidence: Confidence,
}

impl FactMergeEntities {
    pub fn new(
        winner: FactEntityTarget,
        losers: Vec<FactEntityTarget>,
        evidence_facts: Vec<FactTarget>,
        confidence: Confidence,
    ) -> FactLineageResult<Self> {
        validate_entity_merge(&winner, &losers)?;
        Ok(Self {
            winner,
            losers,
            evidence_facts,
            confidence,
        })
    }

    pub fn winner(&self) -> &FactEntityTarget {
        &self.winner
    }

    pub fn losers(&self) -> &[FactEntityTarget] {
        &self.losers
    }

    pub fn evidence_facts(&self) -> &[FactTarget] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactAddAlias {
    entity: FactEntityTarget,
    alias: String,
    evidence_facts: Vec<FactTarget>,
    confidence: Confidence,
}

impl FactAddAlias {
    pub fn new(
        entity: FactEntityTarget,
        alias: String,
        evidence_facts: Vec<FactTarget>,
        confidence: Confidence,
    ) -> FactLineageResult<Self> {
        validate_text(&alias, "compatibility curation alias")?;
        Ok(Self {
            entity,
            alias,
            evidence_facts,
            confidence,
        })
    }

    pub fn entity(&self) -> &FactEntityTarget {
        &self.entity
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn evidence_facts(&self) -> &[FactTarget] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FactLink {
    source: FactTarget,
    target: FactTarget,
    relation: FactRelation,
    evidence_facts: Vec<FactTarget>,
    confidence: Confidence,
    source_label: String,
    metadata: Value,
}

impl FactLink {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: FactTarget,
        target: FactTarget,
        relation: FactRelation,
        evidence_facts: Vec<FactTarget>,
        confidence: Confidence,
        source_label: String,
        metadata: Value,
    ) -> FactLineageResult<Self> {
        if source == target {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility curation relation endpoints",
            }));
        }
        validate_text(&source_label, "compatibility curation relation source")?;
        validate_metadata(&metadata, "compatibility curation relation metadata")?;
        Ok(Self {
            source,
            target,
            relation,
            evidence_facts,
            confidence,
            source_label,
            metadata,
        })
    }

    pub fn source(&self) -> &FactTarget {
        &self.source
    }

    pub fn target(&self) -> &FactTarget {
        &self.target
    }

    pub fn relation(&self) -> FactRelation {
        self.relation
    }

    pub fn evidence_facts(&self) -> &[FactTarget] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub fn metadata(&self) -> &Value {
        &self.metadata
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactRepairVector {
    fact: FactTarget,
    evidence_facts: Vec<FactTarget>,
    confidence: Confidence,
}

impl FactRepairVector {
    pub fn new(fact: FactTarget, evidence_facts: Vec<FactTarget>, confidence: Confidence) -> Self {
        Self {
            fact,
            evidence_facts,
            confidence,
        }
    }

    pub fn fact(&self) -> &FactTarget {
        &self.fact
    }

    pub fn evidence_facts(&self) -> &[FactTarget] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

/// Finite set of curation operations; this is intentionally not a generic
/// command dispatcher.
#[derive(Clone, Debug, PartialEq)]
pub enum FactCurationOperation {
    NormalizeTags(FactNormalizeTags),
    MergeEntities(FactMergeEntities),
    AddAlias(FactAddAlias),
    LinkFacts(FactLink),
    RepairVector(FactRepairVector),
}

impl FactCurationOperation {
    fn validate_for(
        &self,
        owner: &FactOwnerV1,
        min_confidence: Confidence,
    ) -> FactLineageResult<()> {
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
pub struct FactCurationBatch {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
    min_confidence: Confidence,
    operations: Vec<FactCurationOperation>,
}

impl FactCurationBatch {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        actor: Option<ActorId>,
        min_confidence: Confidence,
        operations: Vec<FactCurationOperation>,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        validate_limit(operations.len(), MAX_FACT_CURATION_OPERATIONS)?;
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

    pub fn operations(&self) -> &[FactCurationOperation] {
        &self.operations
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactCurationReceipt {
    owner: FactOwnerV1,
    changed_facts: Vec<FactMapping>,
    normalized_tags: u64,
    merged_entities: u64,
    aliases_added: u64,
    facts_linked: u64,
    vectors_repaired: u64,
    derived_repair: MemoryRepairStats,
}

impl FactCurationReceipt {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        changed_facts: Vec<FactMapping>,
        normalized_tags: u64,
        merged_entities: u64,
        aliases_added: u64,
        facts_linked: u64,
        vectors_repaired: u64,
        derived_repair: MemoryRepairStats,
    ) -> FactLineageResult<Self> {
        owner.validate()?;
        if changed_facts.len() > MAX_FACT_CURATION_TARGETS
            || changed_facts
                .iter()
                .any(|mapping| mapping.owner() != &owner)
            || changed_facts.iter().enumerate().any(|(index, mapping)| {
                changed_facts[..index]
                    .iter()
                    .any(|previous| previous.fact_id() == mapping.fact_id())
            })
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
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

    pub fn changed_facts(&self) -> &[FactMapping] {
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

    pub fn derived_repair(&self) -> &MemoryRepairStats {
        &self.derived_repair
    }
}

fn validate_entity_merge(
    winner: &FactEntityTarget,
    losers: &[FactEntityTarget],
) -> FactLineageResult<()> {
    if losers.is_empty() || losers.len() > MAX_FACT_CURATION_TARGETS {
        return Err(FactLineageError::InvalidQueryLimit {
            limit: losers.len(),
            max: MAX_FACT_CURATION_TARGETS,
        });
    }
    for (index, loser) in losers.iter().enumerate() {
        if loser.owner() != winner.owner()
            || loser == winner
            || losers[..index].iter().any(|previous| previous == loser)
        {
            return Err(FactLineageError::Contract(DomainError::NonCanonical {
                field: "compatibility curation entity merge",
            }));
        }
    }
    Ok(())
}
