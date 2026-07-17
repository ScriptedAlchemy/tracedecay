use serde_json::Value;
use tracedecay_domain::{
    ActorId, Confidence, DomainError, FactCategoryV1, FactEventId, FactOwnerV1, ProvenanceId,
};

use super::super::queries::validate_limit;
use super::super::{
    CompatibilityFactFeedbackActionV1, CompatibilityMemoryRepairStatsV1, FactStoreError,
    FactStoreResult, MAX_COMPATIBILITY_REASON_BYTES,
};
use super::{
    CompatibilityFactIdV1, CompatibilityFactMappingV1, CompatibilityFactProjectionV1,
    CompatibilityFactTargetV1, validate_compatibility_metadata, validate_compatibility_text,
};

const MAX_COMPATIBILITY_CURATION_OPERATIONS: usize = 256;

pub(super) const MAX_COMPATIBILITY_CURATION_TARGETS: usize = 256;

/// Stable, owner-scoped identity for a historical integer entity row. This is
/// only a compatibility target; it is never derived from a path or label.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompatibilityLegacyEntityTargetV1 {
    owner: FactOwnerV1,
    legacy_entity_id: i64,
}

impl CompatibilityLegacyEntityTargetV1 {
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

    pub(super) fn validate(&self) -> FactStoreResult<()> {
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
pub enum CompatibilityFactRelationV1 {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactNormalizeTagsV1 {
    fact: CompatibilityFactTargetV1,
    tags: Vec<String>,
    evidence_facts: Vec<CompatibilityFactTargetV1>,
    confidence: Confidence,
}

impl CompatibilityFactNormalizeTagsV1 {
    pub fn new(
        fact: CompatibilityFactTargetV1,
        tags: Vec<String>,
        evidence_facts: Vec<CompatibilityFactTargetV1>,
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

    pub fn fact(&self) -> &CompatibilityFactTargetV1 {
        &self.fact
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn evidence_facts(&self) -> &[CompatibilityFactTargetV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactMergeEntitiesV1 {
    winner: CompatibilityLegacyEntityTargetV1,
    losers: Vec<CompatibilityLegacyEntityTargetV1>,
    evidence_facts: Vec<CompatibilityFactTargetV1>,
    confidence: Confidence,
}

impl CompatibilityFactMergeEntitiesV1 {
    pub fn new(
        winner: CompatibilityLegacyEntityTargetV1,
        losers: Vec<CompatibilityLegacyEntityTargetV1>,
        evidence_facts: Vec<CompatibilityFactTargetV1>,
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

    pub fn winner(&self) -> &CompatibilityLegacyEntityTargetV1 {
        &self.winner
    }

    pub fn losers(&self) -> &[CompatibilityLegacyEntityTargetV1] {
        &self.losers
    }

    pub fn evidence_facts(&self) -> &[CompatibilityFactTargetV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactAddAliasV1 {
    entity: CompatibilityLegacyEntityTargetV1,
    alias: String,
    evidence_facts: Vec<CompatibilityFactTargetV1>,
    confidence: Confidence,
}

impl CompatibilityFactAddAliasV1 {
    pub fn new(
        entity: CompatibilityLegacyEntityTargetV1,
        alias: String,
        evidence_facts: Vec<CompatibilityFactTargetV1>,
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

    pub fn entity(&self) -> &CompatibilityLegacyEntityTargetV1 {
        &self.entity
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }

    pub fn evidence_facts(&self) -> &[CompatibilityFactTargetV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompatibilityFactLinkV1 {
    source: CompatibilityFactTargetV1,
    target: CompatibilityFactTargetV1,
    relation: CompatibilityFactRelationV1,
    evidence_facts: Vec<CompatibilityFactTargetV1>,
    confidence: Confidence,
    source_label: String,
    metadata: Value,
}

impl CompatibilityFactLinkV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source: CompatibilityFactTargetV1,
        target: CompatibilityFactTargetV1,
        relation: CompatibilityFactRelationV1,
        evidence_facts: Vec<CompatibilityFactTargetV1>,
        confidence: Confidence,
        source_label: String,
        metadata: Value,
    ) -> FactStoreResult<Self> {
        if source == target {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility curation relation endpoints",
            }));
        }
        validate_compatibility_text(&source_label, "compatibility curation relation source")?;
        validate_compatibility_metadata(&metadata, "compatibility curation relation metadata")?;
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

    pub fn source(&self) -> &CompatibilityFactTargetV1 {
        &self.source
    }

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
        &self.target
    }

    pub fn relation(&self) -> CompatibilityFactRelationV1 {
        self.relation
    }

    pub fn evidence_facts(&self) -> &[CompatibilityFactTargetV1] {
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
pub struct CompatibilityFactRepairVectorV1 {
    fact: CompatibilityFactTargetV1,
    evidence_facts: Vec<CompatibilityFactTargetV1>,
    confidence: Confidence,
}

impl CompatibilityFactRepairVectorV1 {
    pub fn new(
        fact: CompatibilityFactTargetV1,
        evidence_facts: Vec<CompatibilityFactTargetV1>,
        confidence: Confidence,
    ) -> Self {
        Self {
            fact,
            evidence_facts,
            confidence,
        }
    }

    pub fn fact(&self) -> &CompatibilityFactTargetV1 {
        &self.fact
    }

    pub fn evidence_facts(&self) -> &[CompatibilityFactTargetV1] {
        &self.evidence_facts
    }

    pub fn confidence(&self) -> Confidence {
        self.confidence
    }
}

/// Finite set of curation operations; this is intentionally not a generic
/// command dispatcher.
#[derive(Clone, Debug, PartialEq)]
pub enum CompatibilityFactCurationOperationV1 {
    NormalizeTags(CompatibilityFactNormalizeTagsV1),
    MergeEntities(CompatibilityFactMergeEntitiesV1),
    AddAlias(CompatibilityFactAddAliasV1),
    LinkFacts(CompatibilityFactLinkV1),
    RepairVector(CompatibilityFactRepairVectorV1),
}

impl CompatibilityFactCurationOperationV1 {
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
pub struct CompatibilityFactCurationBatchV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
    min_confidence: Confidence,
    operations: Vec<CompatibilityFactCurationOperationV1>,
}

impl CompatibilityFactCurationBatchV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        actor: Option<ActorId>,
        min_confidence: Confidence,
        operations: Vec<CompatibilityFactCurationOperationV1>,
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

    pub fn operations(&self) -> &[CompatibilityFactCurationOperationV1] {
        &self.operations
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactCurationReceiptV1 {
    owner: FactOwnerV1,
    changed_facts: Vec<CompatibilityFactMappingV1>,
    normalized_tags: u64,
    merged_entities: u64,
    aliases_added: u64,
    facts_linked: u64,
    vectors_repaired: u64,
    derived_repair: CompatibilityMemoryRepairStatsV1,
}

impl CompatibilityFactCurationReceiptV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        changed_facts: Vec<CompatibilityFactMappingV1>,
        normalized_tags: u64,
        merged_entities: u64,
        aliases_added: u64,
        facts_linked: u64,
        vectors_repaired: u64,
        derived_repair: CompatibilityMemoryRepairStatsV1,
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

    pub fn changed_facts(&self) -> &[CompatibilityFactMappingV1] {
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

    pub fn derived_repair(&self) -> &CompatibilityMemoryRepairStatsV1 {
        &self.derived_repair
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactMergeCommandV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    winner: CompatibilityFactTargetV1,
    losers: Vec<CompatibilityFactTargetV1>,
    merged_content: Option<String>,
    actor: Option<ActorId>,
}

impl CompatibilityFactMergeCommandV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        winner: CompatibilityFactTargetV1,
        losers: Vec<CompatibilityFactTargetV1>,
        merged_content: Option<String>,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        validate_curation_fact_target(&owner, &winner)?;
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        if let Some(content) = &merged_content {
            validate_compatibility_text(content, "compatibility merge content")?;
        }
        if losers.is_empty() || losers.len() > MAX_COMPATIBILITY_CURATION_TARGETS {
            return Err(FactStoreError::InvalidQueryLimit {
                limit: losers.len(),
                max: MAX_COMPATIBILITY_CURATION_TARGETS,
            });
        }
        for (index, loser) in losers.iter().enumerate() {
            validate_curation_fact_target(&owner, loser)?;
            if loser == &winner || losers[..index].iter().any(|previous| previous == loser) {
                return Err(FactStoreError::Contract(DomainError::NonCanonical {
                    field: "compatibility merge targets",
                }));
            }
        }
        Ok(Self {
            owner,
            operation_id,
            winner,
            losers,
            merged_content,
            actor,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }

    pub fn winner(&self) -> &CompatibilityFactTargetV1 {
        &self.winner
    }

    pub fn losers(&self) -> &[CompatibilityFactTargetV1] {
        &self.losers
    }

    pub fn merged_content(&self) -> Option<&str> {
        self.merged_content.as_deref()
    }

    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactMergeOutcomeV1 {
    owner: FactOwnerV1,
    winner: CompatibilityFactMappingV1,
    content_updated: bool,
    deleted_losers: Vec<CompatibilityFactMappingV1>,
}

impl CompatibilityFactMergeOutcomeV1 {
    pub fn new(
        owner: FactOwnerV1,
        winner: CompatibilityFactMappingV1,
        content_updated: bool,
        deleted_losers: Vec<CompatibilityFactMappingV1>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        if winner.owner() != &owner
            || deleted_losers.len() > MAX_COMPATIBILITY_CURATION_TARGETS
            || deleted_losers
                .iter()
                .any(|mapping| mapping.owner() != &owner)
            || deleted_losers
                .iter()
                .any(|mapping| mapping.fact_id() == winner.fact_id())
            || deleted_losers.iter().enumerate().any(|(index, mapping)| {
                deleted_losers[..index]
                    .iter()
                    .any(|previous| previous.fact_id() == mapping.fact_id())
            })
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility merge outcome mappings",
            }));
        }
        Ok(Self {
            owner,
            winner,
            content_updated,
            deleted_losers,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    pub fn winner(&self) -> &CompatibilityFactMappingV1 {
        &self.winner
    }

    pub fn content_updated(&self) -> bool {
        self.content_updated
    }

    pub fn deleted_losers(&self) -> &[CompatibilityFactMappingV1] {
        &self.deleted_losers
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityMemoryRepairCommandV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    actor: Option<ActorId>,
}

impl CompatibilityMemoryRepairCommandV1 {
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            owner,
            operation_id,
            actor,
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
}

/// Daemon-owned, bounded advancement of the persisted V1 raw-memory cutover.
///
/// The implementation captures its source frontier once, advances at most one
/// fixed-size batch, and persists the cursor/quarantine state. Callers retry
/// only while the returned progress is incomplete; they never query V1 rows
/// themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityLegacyMemoryCutoverCommandV1 {
    owner: FactOwnerV1,
    receipt_id: ProvenanceId,
}

impl CompatibilityLegacyMemoryCutoverCommandV1 {
    pub fn new(owner: FactOwnerV1, receipt_id: ProvenanceId) -> FactStoreResult<Self> {
        owner.validate()?;
        receipt_id.validate()?;
        Ok(Self { owner, receipt_id })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }

    /// Stable identity for the persisted cutover receipt. Retries of one
    /// daemon job must retain it so a completed cutover can replay safely.
    pub fn receipt_id(&self) -> &ProvenanceId {
        &self.receipt_id
    }
}

fn validate_curation_confidence(
    confidence: Confidence,
    min_confidence: Confidence,
) -> FactStoreResult<()> {
    if confidence.as_f64() < min_confidence.as_f64() {
        return Err(FactStoreError::Contract(DomainError::NonCanonical {
            field: "compatibility curation confidence",
        }));
    }
    Ok(())
}

fn validate_curation_fact_target(
    owner: &FactOwnerV1,
    target: &CompatibilityFactTargetV1,
) -> FactStoreResult<()> {
    if target.owner() != owner {
        return Err(FactStoreError::OwnerMismatch);
    }
    Ok(())
}

fn validate_curation_entity_target(
    owner: &FactOwnerV1,
    target: &CompatibilityLegacyEntityTargetV1,
) -> FactStoreResult<()> {
    if target.owner() != owner {
        return Err(FactStoreError::OwnerMismatch);
    }
    Ok(())
}

fn validate_curation_evidence(
    owner: &FactOwnerV1,
    evidence_facts: &[CompatibilityFactTargetV1],
) -> FactStoreResult<()> {
    if evidence_facts.is_empty() || evidence_facts.len() > MAX_COMPATIBILITY_CURATION_TARGETS {
        return Err(FactStoreError::InvalidQueryLimit {
            limit: evidence_facts.len(),
            max: MAX_COMPATIBILITY_CURATION_TARGETS,
        });
    }
    for (index, evidence) in evidence_facts.iter().enumerate() {
        validate_curation_fact_target(owner, evidence)?;
        if evidence_facts[..index]
            .iter()
            .any(|previous| previous == evidence)
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility curation evidence",
            }));
        }
    }
    Ok(())
}

fn validate_entity_merge(
    winner: &CompatibilityLegacyEntityTargetV1,
    losers: &[CompatibilityLegacyEntityTargetV1],
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactAddCommandV1 {
    owner: FactOwnerV1,
    operation_id: ProvenanceId,
    content: String,
    category: FactCategoryV1,
    source: Option<String>,
    tags: Vec<String>,
    entities: Vec<String>,
    metadata: Value,
    /// Durable automation identity. This is command metadata, deliberately
    /// separate from the fact payload metadata that passes through privacy
    /// sanitization.
    automation_run_id: Option<String>,
    default_trust: Confidence,
    actor: Option<ActorId>,
}

impl CompatibilityFactAddCommandV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        owner: FactOwnerV1,
        operation_id: ProvenanceId,
        content: String,
        category: FactCategoryV1,
        source: Option<String>,
        tags: Vec<String>,
        entities: Vec<String>,
        metadata: Value,
        default_trust: Confidence,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        owner.validate()?;
        operation_id.validate()?;
        if content.trim().is_empty() {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact add content",
            }));
        }
        if source.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact add source",
            }));
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            owner,
            operation_id,
            content,
            category,
            source,
            tags,
            entities,
            metadata,
            automation_run_id: None,
            default_trust,
            actor,
        })
    }

    pub fn owner(&self) -> &FactOwnerV1 {
        &self.owner
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn content(&self) -> &str {
        &self.content
    }
    pub fn category(&self) -> FactCategoryV1 {
        self.category
    }
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
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
    pub fn with_automation_run_id(mut self, run_id: String) -> FactStoreResult<Self> {
        validate_compatibility_text(&run_id, "compatibility fact automation run identity")?;
        self.automation_run_id = Some(run_id);
        Ok(self)
    }
    pub fn automation_run_id(&self) -> Option<&str> {
        self.automation_run_id.as_deref()
    }
    pub fn default_trust(&self) -> Confidence {
        self.default_trust
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactUpdatePatchV1 {
    content: Option<String>,
    category: Option<FactCategoryV1>,
    source: Option<Option<String>>,
    tags: Option<Vec<String>>,
    entities: Option<Vec<String>>,
    metadata: Option<Value>,
    trust: Option<Confidence>,
}

impl CompatibilityFactUpdatePatchV1 {
    pub fn new(
        content: Option<String>,
        category: Option<FactCategoryV1>,
        source: Option<Option<String>>,
        tags: Option<Vec<String>>,
        entities: Option<Vec<String>>,
        metadata: Option<Value>,
        trust: Option<Confidence>,
    ) -> FactStoreResult<Self> {
        if content.is_none()
            && category.is_none()
            && source.is_none()
            && tags.is_none()
            && entities.is_none()
            && metadata.is_none()
            && trust.is_none()
        {
            return Err(FactStoreError::Contract(DomainError::Empty {
                field: "compatibility fact update patch",
            }));
        }
        if content
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact update content",
            }));
        }
        if source.as_ref().is_some_and(|value| {
            value.as_ref().is_some_and(|source| {
                source.trim().is_empty() || source.len() > MAX_COMPATIBILITY_REASON_BYTES
            })
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact update source",
            }));
        }
        Ok(Self {
            content,
            category,
            source,
            tags,
            entities,
            metadata,
            trust,
        })
    }

    pub fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }
    pub fn category(&self) -> Option<FactCategoryV1> {
        self.category
    }
    pub fn source(&self) -> Option<Option<&str>> {
        self.source.as_ref().map(|value| value.as_deref())
    }
    pub fn tags(&self) -> Option<&[String]> {
        self.tags.as_deref()
    }
    pub fn entities(&self) -> Option<&[String]> {
        self.entities.as_deref()
    }
    pub fn metadata(&self) -> Option<&Value> {
        self.metadata.as_ref()
    }
    pub fn trust(&self) -> Option<Confidence> {
        self.trust
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactUpdateCommandV1 {
    target: CompatibilityFactTargetV1,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    patch: CompatibilityFactUpdatePatchV1,
    actor: Option<ActorId>,
}

impl CompatibilityFactUpdateCommandV1 {
    pub fn new(
        target: CompatibilityFactTargetV1,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        patch: CompatibilityFactUpdatePatchV1,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            patch,
            actor,
        })
    }

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn patch(&self) -> &CompatibilityFactUpdatePatchV1 {
        &self.patch
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactRemoveCommandV1 {
    target: CompatibilityFactTargetV1,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    actor: Option<ActorId>,
}

impl CompatibilityFactRemoveCommandV1 {
    pub fn new(
        target: CompatibilityFactTargetV1,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        actor: Option<ActorId>,
    ) -> FactStoreResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            actor,
        })
    }

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactFeedbackCommandV1 {
    target: CompatibilityFactTargetV1,
    operation_id: ProvenanceId,
    expected_last_event_id: Option<FactEventId>,
    action: CompatibilityFactFeedbackActionV1,
    actor: Option<ActorId>,
    source: Option<String>,
    reason: Option<String>,
}

impl CompatibilityFactFeedbackCommandV1 {
    pub fn new(
        target: CompatibilityFactTargetV1,
        operation_id: ProvenanceId,
        expected_last_event_id: Option<FactEventId>,
        action: CompatibilityFactFeedbackActionV1,
        actor: Option<ActorId>,
        source: Option<String>,
        reason: Option<String>,
    ) -> FactStoreResult<Self> {
        operation_id.validate()?;
        if let Some(event_id) = &expected_last_event_id {
            event_id.validate()?;
        }
        if let Some(actor) = &actor {
            actor.validate()?;
        }
        if source.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact feedback source",
            }));
        }
        if reason.as_ref().is_some_and(|value| {
            value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
        }) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact feedback reason",
            }));
        }
        Ok(Self {
            target,
            operation_id,
            expected_last_event_id,
            action,
            actor,
            source,
            reason,
        })
    }

    pub fn target(&self) -> &CompatibilityFactTargetV1 {
        &self.target
    }
    pub fn operation_id(&self) -> &ProvenanceId {
        &self.operation_id
    }
    pub fn expected_last_event_id(&self) -> Option<&FactEventId> {
        self.expected_last_event_id.as_ref()
    }
    pub fn action(&self) -> CompatibilityFactFeedbackActionV1 {
        self.action
    }
    pub fn actor(&self) -> Option<&ActorId> {
        self.actor.as_ref()
    }
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompatibilityFactAddDispositionV1 {
    Added,
    NearDuplicate,
    PossibleConflict,
    RejectedSecretLike,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactAddOutcomeV1 {
    fact: Option<CompatibilityFactProjectionV1>,
    disposition: CompatibilityFactAddDispositionV1,
    closest_fact_id: Option<CompatibilityFactIdV1>,
    similarity_millionths: Option<u32>,
    reason: Option<String>,
}

impl CompatibilityFactAddOutcomeV1 {
    pub fn new(
        fact: Option<CompatibilityFactProjectionV1>,
        disposition: CompatibilityFactAddDispositionV1,
        closest_fact_id: Option<CompatibilityFactIdV1>,
        similarity_millionths: Option<u32>,
        reason: Option<String>,
    ) -> FactStoreResult<Self> {
        if similarity_millionths.is_some_and(|value| value > 1_000_000)
            || reason.as_ref().is_some_and(|value| {
                value.trim().is_empty() || value.len() > MAX_COMPATIBILITY_REASON_BYTES
            })
        {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact add outcome",
            }));
        }
        Ok(Self {
            fact,
            disposition,
            closest_fact_id,
            similarity_millionths,
            reason,
        })
    }

    pub fn fact(&self) -> Option<&CompatibilityFactProjectionV1> {
        self.fact.as_ref()
    }
    pub fn disposition(&self) -> CompatibilityFactAddDispositionV1 {
        self.disposition
    }
    pub fn closest_fact_id(&self) -> Option<&CompatibilityFactIdV1> {
        self.closest_fact_id.as_ref()
    }
    pub fn similarity_millionths(&self) -> Option<u32> {
        self.similarity_millionths
    }
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactUpdateOutcomeV1 {
    fact: CompatibilityFactProjectionV1,
    trust_delta_millionths: i32,
}

impl CompatibilityFactUpdateOutcomeV1 {
    pub fn new(
        fact: CompatibilityFactProjectionV1,
        trust_delta_millionths: i32,
    ) -> FactStoreResult<Self> {
        if !(-1_000_000..=1_000_000).contains(&trust_delta_millionths) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact update trust delta",
            }));
        }
        Ok(Self {
            fact,
            trust_delta_millionths,
        })
    }

    pub fn fact(&self) -> &CompatibilityFactProjectionV1 {
        &self.fact
    }
    pub fn trust_delta_millionths(&self) -> i32 {
        self.trust_delta_millionths
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactRemoveOutcomeV1 {
    fact: CompatibilityFactProjectionV1,
    removed: bool,
    remaining_fact_count: u64,
}

impl CompatibilityFactRemoveOutcomeV1 {
    pub fn new(
        fact: CompatibilityFactProjectionV1,
        removed: bool,
        remaining_fact_count: u64,
    ) -> Self {
        Self {
            fact,
            removed,
            remaining_fact_count,
        }
    }

    pub fn fact(&self) -> &CompatibilityFactProjectionV1 {
        &self.fact
    }
    pub fn removed(&self) -> bool {
        self.removed
    }
    pub fn remaining_fact_count(&self) -> u64 {
        self.remaining_fact_count
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompatibilityFactFeedbackOutcomeV1 {
    fact: CompatibilityFactProjectionV1,
    event_id: FactEventId,
    /// Numeric event identity from the authoritative V1 mirror.  It is only
    /// present when the adapter durably recorded that mirror row; callers must
    /// not derive it from the canonical event identifier.
    legacy_feedback_event_id: Option<i64>,
    old_trust: Confidence,
    new_trust: Confidence,
    trust_delta_millionths: i32,
    helpful_count: u64,
    unhelpful_count: u64,
}

impl CompatibilityFactFeedbackOutcomeV1 {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fact: CompatibilityFactProjectionV1,
        event_id: FactEventId,
        legacy_feedback_event_id: Option<i64>,
        old_trust: Confidence,
        new_trust: Confidence,
        trust_delta_millionths: i32,
        helpful_count: u64,
        unhelpful_count: u64,
    ) -> FactStoreResult<Self> {
        event_id.validate()?;
        if legacy_feedback_event_id.is_some_and(|value| value <= 0) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility legacy feedback event id",
            }));
        }
        if !(-1_000_000..=1_000_000).contains(&trust_delta_millionths) {
            return Err(FactStoreError::Contract(DomainError::NonCanonical {
                field: "compatibility fact feedback trust delta",
            }));
        }
        Ok(Self {
            fact,
            event_id,
            legacy_feedback_event_id,
            old_trust,
            new_trust,
            trust_delta_millionths,
            helpful_count,
            unhelpful_count,
        })
    }

    pub fn fact(&self) -> &CompatibilityFactProjectionV1 {
        &self.fact
    }
    pub fn event_id(&self) -> &FactEventId {
        &self.event_id
    }
    pub fn legacy_feedback_event_id(&self) -> Option<i64> {
        self.legacy_feedback_event_id
    }
    pub fn old_trust(&self) -> Confidence {
        self.old_trust
    }
    pub fn new_trust(&self) -> Confidence {
        self.new_trust
    }
    pub fn trust_delta_millionths(&self) -> i32 {
        self.trust_delta_millionths
    }
    pub fn helpful_count(&self) -> u64 {
        self.helpful_count
    }
    pub fn unhelpful_count(&self) -> u64 {
        self.unhelpful_count
    }
}
